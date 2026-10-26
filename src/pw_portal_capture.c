// XDG Desktop Portal ScreenCast + PipeWire capture helper for HyperGSR.
//
// The helper owns the portal session and the PipeWire stream, and feeds raw
// frames to an FFmpeg child whose output arguments come from the recorder.
// Frames are stamped with the wall clock as FFmpeg reads them, so the output
// always lasts as long as the recording did, even when the compositor only
// sends frames on damage or the encoder briefly falls behind.
//
// Usage:
//   hypergsr-pw-capture --protocol
//   hypergsr-pw-capture [--token FILE] [--fps N] [--sources monitor|window|both]
//                       [--cursor embedded|hidden|metadata] [--cursor-track FILE]
//                       [--reset] -- <ffmpeg output args>
//
// With --cursor metadata the video has no cursor; its position and image go
// to the --cursor-track file instead, one line per change:
//   HGSR-CURSOR 1                       header
//   S <id> <w> <h> <hot_x> <hot_y> <hex RGBA, straight alpha>   a new image
//   P <ns> <x> <y> <id>                 hotspot at stream pixel (x, y) from
//                                       CLOCK_MONOTONIC <ns>; id 0 is hidden
//   T <ns> <w> <h>                      CLOCK_MONOTONIC time of video time zero,
//                                       and the stream size
//
// stdout carries one line per event:
//   READY <width> <height>   the first frame went to FFmpeg (video time zero)
//   STALLED <sec>            no new image (or metadata cursor move) for a while;
//                            <sec> is the video time of the last one (a still
//                            screen also stalls)
//   FLOWING                  updates arrive again after STALLED
//   LOST <sec> <message>     the screen-cast stream went away mid-recording; the
//                            file is finalized, and is worth keeping up to <sec>.
//                            The exit code is EXIT_STREAM_LOST.
//   ERROR <message>          the capture failed; the exit code is non-zero
// Closing stdin, SIGINT, or SIGTERM stops the capture and finalizes the file.

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include <gio/gio.h>
#include <glib-unix.h>
#include <glib.h>

#include <pipewire/pipewire.h>
#include <spa/buffer/meta.h>
#include <spa/param/buffers.h>
#include <spa/param/video/format-utils.h>

#define HELPER_PROTOCOL 5

// libportal.so.1 ABI, declared here so only the shared library is needed.
typedef struct _XdpPortal XdpPortal;
typedef struct _XdpSession XdpSession;
typedef struct _XdpParent XdpParent;

extern XdpPortal *xdp_portal_new(void);
extern void xdp_portal_create_screencast_session(XdpPortal *portal, int outputs, int flags,
                                                 int cursor_mode, int persist_mode,
                                                 const char *restore_token,
                                                 GCancellable *cancellable,
                                                 GAsyncReadyCallback callback, gpointer data);
extern XdpSession *xdp_portal_create_screencast_session_finish(XdpPortal *portal,
                                                               GAsyncResult *result,
                                                               GError **error);
extern void xdp_session_start(XdpSession *session, XdpParent *parent,
                              GCancellable *cancellable, GAsyncReadyCallback callback,
                              gpointer data);
extern gboolean xdp_session_start_finish(XdpSession *session, GAsyncResult *result,
                                         GError **error);
extern const char *xdp_session_get_restore_token(XdpSession *session);
extern GVariant *xdp_session_get_streams(XdpSession *session);
extern int xdp_session_open_pipewire_remote(XdpSession *session);
extern void xdp_session_close(XdpSession *session);

enum {
    XDP_OUTPUT_MONITOR = 1,
    XDP_OUTPUT_WINDOW = 2,
    XDP_CURSOR_MODE_HIDDEN = 1,
    XDP_CURSOR_MODE_EMBEDDED = 2,
    XDP_CURSOR_MODE_METADATA = 4,
    XDP_PERSIST_MODE_PERSISTENT = 2,
};

enum {
    EXIT_PORTAL = 2,
    EXIT_PIPEWIRE = 3,
    EXIT_ENCODER = 4,
    EXIT_NO_FRAMES = 5,
    EXIT_STREAM_LOST = 6,
};

// Seconds to wait for the first frame once the stream is connected.
#define FIRST_FRAME_TIMEOUT_S 15
// Re-send the current frame at least this often so a crash loses little and
// FFmpeg's constant-frame-rate output never stalls on a still screen.
#define KEEPALIVE_NS 250000000ULL
// Report STALLED after this long without a new image.
#define STALL_REPORT_NS 3000000000ULL
// A stream that stays paused this long after it was streaming has lost its
// source; renegotiation pauses it only briefly.
#define PAUSED_LOST_NS 2000000000ULL
// Largest cursor image kept from the stream's cursor metadata.
#define CURSOR_MAX 256
#define CURSOR_META_SIZE(w, h)                                                     \
    (sizeof(struct spa_meta_cursor) + sizeof(struct spa_meta_bitmap) + (w) * (h) * 4)

static volatile sig_atomic_t g_stop_requested = 0;
static volatile sig_atomic_t g_stream_lost = 0;
static char g_lost_reason[256];
static int g_stop_pipe[2] = {-1, -1};

static void handle_stop_signal(int sig) {
    (void)sig;
    g_stop_requested = 1;
    if (g_stop_pipe[1] >= 0) {
        const char ch = 'q';
        ssize_t r = write(g_stop_pipe[1], &ch, 1);
        (void)r;
    }
}

// The source of a running stream went away (the compositor's portal crashed
// or ended the share). The writer finalizes the file and reports LOST.
static void stream_lost(const char *reason) {
    if (!g_stream_lost) {
        snprintf(g_lost_reason, sizeof(g_lost_reason), "%s", reason);
        __atomic_store_n(&g_stream_lost, 1, __ATOMIC_RELEASE);
    }
    handle_stop_signal(0);
}

typedef struct {
    GMainLoop *glib_loop;
    XdpPortal *portal;
    XdpSession *session;
    const char *token_path;
    bool reset_token;
    int sources;
    int cursor_mode;
    int target_fps;
    char **ffmpeg_tail;
    int ffmpeg_tail_len;

    uint32_t pw_node_id;
    int pw_fd;
    bool portal_ok;

    struct pw_main_loop *pw_loop;
    struct pw_context *pw_ctx;
    struct pw_core *pw_core;
    struct pw_stream *pw_stream;
    struct spa_hook stream_listener;
    struct pw_registry *pw_registry;
    struct spa_hook registry_listener;
    struct spa_video_info format;
    // Set by the PipeWire thread, read by the writer thread.
    bool ever_streamed;
    uint64_t paused_since_ns;

    pthread_mutex_t frame_lock;
    uint8_t *latest_frame;
    size_t latest_frame_size;
    uint32_t frame_w;
    uint32_t frame_h;
    const char *pix_fmt;
    pthread_cond_t frame_ready;
    bool has_first_frame;
    uint64_t frame_serial;

    // Cursor track (metadata mode). Written by the PipeWire thread; the
    // writer thread adds the time-zero line and closes it.
    pthread_mutex_t track_lock;
    FILE *track;
    uint64_t track_flushed_ns;
    uint32_t shape_id;
    uint32_t shape_w, shape_h;
    int32_t shape_hx, shape_hy;
    uint8_t *shape;      // straight-alpha RGBA, shape_w * shape_h * 4
    uint8_t *shape_tmp;  // conversion scratch, CURSOR_MAX^2 * 4
    bool bitmap_hidden;
    int32_t last_x, last_y;
    uint32_t last_id;
    // Counts cursor updates; with a metadata cursor they show the stream
    // is alive while the image is still.
    uint64_t cursor_updates;
} CaptureApp;

// Protocol lines come from the PipeWire and writer threads; one write(2)
// per line keeps them from interleaving.
static void emit(const char *fmt, ...) {
    char line[1024];
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(line, sizeof(line) - 1, fmt, ap);
    va_end(ap);
    if (n < 0) return;
    if ((size_t)n > sizeof(line) - 2) n = (int)sizeof(line) - 2;
    line[n++] = '\n';
    for (const char *p = line; n > 0;) {
        ssize_t w = write(STDOUT_FILENO, p, (size_t)n);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) return;
        p += w;
        n -= (int)w;
    }
}

static void log_err(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    fputs("hypergsr-pw-capture: ", stderr);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    fputc('\n', stderr);
}

static uint64_t monotonic_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

static void sleep_ns(uint64_t ns) {
    struct timespec req = {(time_t)(ns / 1000000000ULL), (long)(ns % 1000000000ULL)};
    while (nanosleep(&req, &req) < 0 && errno == EINTR) {
    }
}

// FFmpeg may stall (a hung GPU encoder); give up after this long without progress.
#define WRITE_STALL_NS 10000000000ULL
// Once a stop is requested, a stalled frame write gets this long to finish.
#define STOP_GRACE_NS 3000000000ULL

static bool stop_signalled(struct pollfd *fds, int nfds);

// Writes a frame to FFmpeg's non-blocking pipe without ever blocking past a
// stop request or a stalled encoder. Sets errno to ETIMEDOUT on a stall.
static bool write_frame(int fd, const uint8_t *buf, size_t len, struct pollfd *stop_fds,
                        int nstop) {
    uint64_t last_progress = monotonic_ns();
    uint64_t stop_at = 0;
    while (len > 0) {
        ssize_t n = write(fd, buf, len);
        if (n > 0) {
            buf += n;
            len -= (size_t)n;
            last_progress = monotonic_ns();
            continue;
        }
        if (n < 0 && errno != EAGAIN && errno != EINTR) return false;
        uint64_t now = monotonic_ns();
        if (!stop_at && stop_signalled(stop_fds, nstop)) stop_at = now;
        if (now - last_progress > WRITE_STALL_NS || (stop_at && now - stop_at > STOP_GRACE_NS)) {
            errno = ETIMEDOUT;
            return false;
        }
        struct pollfd out = {.fd = fd, .events = POLLOUT};
        poll(&out, 1, 100);
    }
    return true;
}

static void save_token(const char *path, const char *token) {
    if (!path || !token || !token[0]) return;
    char tmp[4096];
    snprintf(tmp, sizeof(tmp), "%s.tmp", path);
    FILE *f = fopen(tmp, "w");
    if (!f) return;
    fprintf(f, "%s", token);
    if (fclose(f) == 0) rename(tmp, path);
}

static char *load_token(const char *path) {
    if (!path) return NULL;
    FILE *f = fopen(path, "r");
    if (!f) return NULL;
    char buf[256] = {0};
    char *line = fgets(buf, sizeof(buf) - 1, f);
    fclose(f);
    if (!line) return NULL;
    size_t len = strlen(buf);
    while (len > 0 && (buf[len - 1] == '\n' || buf[len - 1] == '\r' || buf[len - 1] == ' ')) {
        buf[--len] = '\0';
    }
    return len > 0 ? g_strdup(buf) : NULL;
}

// ---------------------------------------------------------------------------
// FFmpeg child
// ---------------------------------------------------------------------------

typedef struct {
    pid_t pid;
    int fd;
    uint32_t w;
    uint32_t h;
} Encoder;

static bool spawn_encoder(CaptureApp *app, Encoder *enc, uint32_t w, uint32_t h,
                          const char *pix_fmt) {
    char size[32], fps[16];
    snprintf(size, sizeof(size), "%ux%u", w, h);
    snprintf(fps, sizeof(fps), "%d", app->target_fps);

    const char *head[] = {
        "ffmpeg", "-hide_banner", "-nostats", "-loglevel", "error", "-y",
        "-f", "rawvideo", "-pix_fmt", pix_fmt, "-video_size", size, "-framerate", fps,
        "-use_wallclock_as_timestamps", "1", "-thread_queue_size", "4", "-i", "pipe:0",
    };
    size_t head_len = sizeof(head) / sizeof(head[0]);
    char **argv = calloc(head_len + (size_t)app->ffmpeg_tail_len + 1, sizeof(char *));
    if (!argv) return false;
    for (size_t i = 0; i < head_len; i++) argv[i] = (char *)head[i];
    for (int i = 0; i < app->ffmpeg_tail_len; i++) argv[head_len + i] = app->ffmpeg_tail[i];

    int fds[2];
    if (pipe2(fds, O_CLOEXEC) < 0) {
        free(argv);
        return false;
    }
    pid_t pid = fork();
    if (pid < 0) {
        close(fds[0]);
        close(fds[1]);
        free(argv);
        return false;
    }
    if (pid == 0) {
        // Own process group: a Ctrl+C in the launching terminal must not cut the
        // file short; the helper decides when FFmpeg stops.
        setpgid(0, 0);
        // Only async-signal-safe calls between fork and exec.
        dup2(fds[0], STDIN_FILENO);
        int devnull = open("/dev/null", O_WRONLY | O_CLOEXEC);
        if (devnull >= 0) dup2(devnull, STDOUT_FILENO);
        signal(SIGPIPE, SIG_DFL);
        execvp("ffmpeg", argv);
        static const char msg[] = "hypergsr-pw-capture: cannot run ffmpeg\n";
        ssize_t r = write(STDERR_FILENO, msg, sizeof(msg) - 1);
        (void)r;
        _exit(127);
    }
    close(fds[0]);
    free(argv);
    // Set the group from both sides so a kill(-pid) right after ENCODER
    // cannot race the child's own setpgid.
    setpgid(pid, pid);
    // A large pipe lets FFmpeg's demuxer pull whole frames in fewer wakeups.
    fcntl(fds[1], F_SETPIPE_SZ, 1 << 20);
    fcntl(fds[1], F_SETFL, fcntl(fds[1], F_GETFL) | O_NONBLOCK);
    // The recorder kills this process group if the helper itself is killed.
    emit("ENCODER %d", (int)pid);
    enc->pid = pid;
    enc->fd = fds[1];
    enc->w = w;
    enc->h = h;
    return true;
}

// Closes FFmpeg's input and waits for it to write the trailer. Returns true
// when FFmpeg exited cleanly.
static bool finish_encoder(Encoder *enc) {
    if (enc->fd >= 0) {
        close(enc->fd);
        enc->fd = -1;
    }
    if (enc->pid <= 0) return true;

    // EOF on the video input ends the file; audio inputs are live, so the
    // recorder passes -shortest. SIGINT is FFmpeg's own graceful stop.
    const struct {
        int signal;
        int wait_ms;
    } steps[] = {{0, 4000}, {SIGINT, 8000}, {SIGKILL, 2000}};
    for (size_t s = 0; s < sizeof(steps) / sizeof(steps[0]); s++) {
        if (steps[s].signal) kill(enc->pid, steps[s].signal);
        for (int waited = 0; waited < steps[s].wait_ms; waited += 20) {
            int status = 0;
            pid_t r = waitpid(enc->pid, &status, WNOHANG);
            if (r == enc->pid) {
                enc->pid = 0;
                if (!WIFEXITED(status)) return false;
                // FFmpeg exits 255 after a SIGINT, having finalized the file.
                int code = WEXITSTATUS(status);
                return code == 0 || (steps[s].signal == SIGINT && code == 255);
            }
            if (r < 0 && errno != EINTR) {
                enc->pid = 0;
                return false;
            }
            sleep_ns(20000000ULL);
        }
    }
    enc->pid = 0;
    return false;
}

// ---------------------------------------------------------------------------
// Writer thread: paces frames into FFmpeg and owns the process exit.
// ---------------------------------------------------------------------------

static bool stop_signalled(struct pollfd *fds, int nfds) {
    if (g_stop_requested) return true;
    if (nfds == 0) return false;
    if (poll(fds, (nfds_t)nfds, 0) > 0) {
        for (int i = 0; i < nfds; i++) {
            if (fds[i].revents & (POLLIN | POLLHUP | POLLERR)) {
                // Latch it, so later checks without the fds (the tail write)
                // still see the stop.
                g_stop_requested = 1;
                return true;
            }
        }
    }
    return false;
}

static void track_time_zero(CaptureApp *app, uint64_t ns, uint32_t w, uint32_t h) {
    pthread_mutex_lock(&app->track_lock);
    if (app->track) {
        fprintf(app->track, "T %llu %u %u\n", (unsigned long long)ns, w, h);
        fflush(app->track);
    }
    pthread_mutex_unlock(&app->track_lock);
}

static void track_close(CaptureApp *app) {
    pthread_mutex_lock(&app->track_lock);
    if (app->track) {
        fclose(app->track);
        app->track = NULL;
    }
    pthread_mutex_unlock(&app->track_lock);
}

static void *encoder_writer_thread(void *arg) {
    CaptureApp *app = (CaptureApp *)arg;

    struct pollfd fds[2];
    int nfds = 0;
    if (g_stop_pipe[0] >= 0) {
        fds[nfds].fd = g_stop_pipe[0];
        fds[nfds].events = POLLIN;
        nfds++;
    }
    struct stat st;
    if (fstat(STDIN_FILENO, &st) == 0 && S_ISFIFO(st.st_mode)) {
        fds[nfds].fd = STDIN_FILENO;
        fds[nfds].events = POLLIN;
        nfds++;
    }

    const uint64_t interval_ns = 1000000000ULL / (uint64_t)app->target_fps;
    const uint64_t started_ns = monotonic_ns();
    Encoder enc = {.pid = 0, .fd = -1, .w = 0, .h = 0};
    uint8_t *buf = NULL;
    size_t buf_size = 0;
    uint32_t buf_w = 0, buf_h = 0;
    const char *buf_fmt = NULL;
    uint64_t sent_serial = 0;
    uint64_t last_write_ns = 0;
    bool warned_resize = false;
    bool partial_write = false;
    uint64_t ready_ns = 0;
    uint64_t last_fresh_ns = 0;
    uint64_t seen_cursor_updates = 0;
    bool stalled = false;
    int exit_code = 0;

    while (!stop_signalled(fds, nfds)) {
        uint64_t now = monotonic_ns();
        if (last_write_ns && now - last_write_ns < interval_ns) {
            sleep_ns(interval_ns - (now - last_write_ns));
            continue;
        }

        bool fresh = false;
        pthread_mutex_lock(&app->frame_lock);
        if (app->frame_serial == sent_serial || !app->has_first_frame) {
            // Sleep until PipeWire delivers a frame, waking regularly to
            // check for a stop request and send keepalives.
            struct timespec deadline;
            clock_gettime(CLOCK_MONOTONIC, &deadline);
            deadline.tv_nsec += 20000000L;
            if (deadline.tv_nsec >= 1000000000L) {
                deadline.tv_sec++;
                deadline.tv_nsec -= 1000000000L;
            }
            pthread_cond_timedwait(&app->frame_ready, &app->frame_lock, &deadline);
        }
        if (app->has_first_frame && app->latest_frame && app->frame_serial != sent_serial) {
            sent_serial = app->frame_serial;
            if (enc.pid > 0 && buf && (app->frame_w != enc.w || app->frame_h != enc.h)) {
                // FFmpeg cannot change raw input size mid-stream (a resized
                // window), so the new image is centered on the recorded size.
                if (!warned_resize) {
                    log_err("stream resized to %ux%u; centering it in %ux%u", app->frame_w,
                            app->frame_h, enc.w, enc.h);
                    warned_resize = true;
                }
                uint32_t cw = app->frame_w < enc.w ? app->frame_w : enc.w;
                uint32_t ch = app->frame_h < enc.h ? app->frame_h : enc.h;
                uint32_t sx = (app->frame_w - cw) / 2, sy = (app->frame_h - ch) / 2;
                uint32_t dx = (enc.w - cw) / 2, dy = (enc.h - ch) / 2;
                memset(buf, 0, buf_size);
                for (uint32_t y = 0; y < ch; y++) {
                    memcpy(buf + ((size_t)(dy + y) * enc.w + dx) * 4,
                           app->latest_frame + ((size_t)(sy + y) * app->frame_w + sx) * 4,
                           (size_t)cw * 4);
                }
                fresh = true;
            } else {
                if (buf_size != app->latest_frame_size) {
                    free(buf);
                    buf_size = app->latest_frame_size;
                    buf = malloc(buf_size);
                }
                if (buf) {
                    memcpy(buf, app->latest_frame, buf_size);
                    buf_w = app->frame_w;
                    buf_h = app->frame_h;
                    buf_fmt = app->pix_fmt;
                    fresh = true;
                }
            }
        }
        pthread_mutex_unlock(&app->frame_lock);

        now = monotonic_ns();
        uint64_t paused = __atomic_load_n(&app->paused_since_ns, __ATOMIC_RELAXED);
        if (paused && now > paused && now - paused > PAUSED_LOST_NS) {
            stream_lost("the screen-cast stream stopped");
            continue;
        }
        if (!buf) {
            if (now - started_ns > (uint64_t)FIRST_FRAME_TIMEOUT_S * 1000000000ULL) {
                emit("ERROR no frames arrived from the screen-cast stream");
                exit_code = EXIT_NO_FRAMES;
                break;
            }
            continue;
        }
        if (enc.pid == 0 && !spawn_encoder(app, &enc, buf_w, buf_h, buf_fmt)) {
            emit("ERROR cannot start ffmpeg: %s", strerror(errno));
            exit_code = EXIT_ENCODER;
            break;
        }

        // Only new frames are sent, plus a keepalive on a still screen; the
        // wall-clock timestamps let FFmpeg fill the gaps at a constant rate.
        if (fresh || now - last_write_ns >= KEEPALIVE_NS) {
            if (!write_frame(enc.fd, buf, (size_t)enc.w * enc.h * 4, fds, nfds)) {
                // A frame was cut short; another full frame after it would
                // misalign the raw stream, so no tail frame is written.
                partial_write = true;
                if (errno == ETIMEDOUT && stop_signalled(fds, nfds)) break;
                emit("ERROR ffmpeg stopped accepting frames (%s)", strerror(errno));
                exit_code = EXIT_ENCODER;
                break;
            }
            if (last_write_ns == 0) {
                track_time_zero(app, now, enc.w, enc.h);
                emit("READY %u %u", enc.w, enc.h);
                ready_ns = now;
            }
            last_write_ns = now;
        }
        uint64_t cursor_updates = __atomic_load_n(&app->cursor_updates, __ATOMIC_RELAXED);
        bool cursor_moved = cursor_updates != seen_cursor_updates;
        seen_cursor_updates = cursor_updates;
        if (fresh || (cursor_moved && ready_ns)) {
            last_fresh_ns = now;
            if (stalled) emit("FLOWING");
            stalled = false;
        } else if (ready_ns && !stalled && now - last_fresh_ns > STALL_REPORT_NS) {
            emit("STALLED %.3f", (double)(last_fresh_ns - ready_ns) / 1e9);
            stalled = true;
        }
    }

    bool lost = __atomic_load_n(&g_stream_lost, __ATOMIC_ACQUIRE);
    // One last copy of the current frame keeps a still tail in the file; a
    // lost stream has frozen already, so it gets none.
    if (exit_code == 0 && enc.fd >= 0 && last_write_ns && !partial_write && !lost) {
        write_frame(enc.fd, buf, (size_t)enc.w * enc.h * 4, fds, nfds);
    }
    bool clean = finish_encoder(&enc);
    if (exit_code == 0 && lost) {
        if (last_write_ns) {
            emit("LOST %.3f %s", (double)(last_fresh_ns - ready_ns) / 1e9, g_lost_reason);
            exit_code = EXIT_STREAM_LOST;
        } else {
            emit("ERROR %s", g_lost_reason);
            exit_code = EXIT_NO_FRAMES;
        }
    }
    if (exit_code == 0 && !clean && last_write_ns) {
        emit("ERROR ffmpeg did not finish the file cleanly");
        exit_code = EXIT_ENCODER;
    }
    free(buf);
    track_close(app);
    fflush(stdout);
    _exit(exit_code);
    return NULL;
}

// ---------------------------------------------------------------------------
// Portal
// ---------------------------------------------------------------------------

static void on_session_started(GObject *source, GAsyncResult *res, gpointer user_data) {
    (void)source;
    CaptureApp *app = (CaptureApp *)user_data;
    GError *err = NULL;
    if (!xdp_session_start_finish(app->session, res, &err)) {
        emit("ERROR screen sharing was cancelled or denied (%s)", err ? err->message : "unknown");
        if (err) g_error_free(err);
        g_main_loop_quit(app->glib_loop);
        return;
    }

    const char *new_tok = xdp_session_get_restore_token(app->session);
    if (new_tok) save_token(app->token_path, new_tok);

    GVariant *streams = xdp_session_get_streams(app->session);
    if (!streams) {
        emit("ERROR the portal returned no screen-cast stream");
        g_main_loop_quit(app->glib_loop);
        return;
    }
    GVariantIter iter;
    g_variant_iter_init(&iter, streams);
    uint32_t node_id = 0;
    GVariant *props = NULL;
    if (g_variant_iter_next(&iter, "(u@a{sv})", &node_id, &props)) {
        app->pw_node_id = node_id;
        if (props) g_variant_unref(props);
    }
    if (app->pw_node_id == 0) {
        emit("ERROR the portal stream has no PipeWire node");
        g_main_loop_quit(app->glib_loop);
        return;
    }

    app->pw_fd = xdp_session_open_pipewire_remote(app->session);
    if (app->pw_fd < 0) {
        emit("ERROR cannot open the PipeWire remote");
        g_main_loop_quit(app->glib_loop);
        return;
    }
    app->portal_ok = true;
    g_main_loop_quit(app->glib_loop);
}

static void on_session_created(GObject *source, GAsyncResult *res, gpointer user_data) {
    (void)source;
    CaptureApp *app = (CaptureApp *)user_data;
    GError *err = NULL;
    app->session = xdp_portal_create_screencast_session_finish(app->portal, res, &err);
    if (!app->session) {
        emit("ERROR cannot create a screen-cast session (%s)", err ? err->message : "unknown");
        if (err) g_error_free(err);
        g_main_loop_quit(app->glib_loop);
        return;
    }
    xdp_session_start(app->session, NULL, NULL, on_session_started, app);
}

// The portal can block in its picker; a stop request must still end the helper.
static gboolean on_stop_during_portal(gint fd, GIOCondition cond, gpointer user_data) {
    (void)fd;
    (void)cond;
    CaptureApp *app = (CaptureApp *)user_data;
    g_stop_requested = 1;
    g_main_loop_quit(app->glib_loop);
    return G_SOURCE_REMOVE;
}

// ---------------------------------------------------------------------------
// PipeWire stream
// ---------------------------------------------------------------------------

static const char *spa_fmt_to_ffmpeg_pix_fmt(uint32_t fmt) {
    switch (fmt) {
    case SPA_VIDEO_FORMAT_RGBx:
    case SPA_VIDEO_FORMAT_RGBA:
        return "rgb0";
    case SPA_VIDEO_FORMAT_xBGR:
    case SPA_VIDEO_FORMAT_ABGR:
        return "0bgr";
    case SPA_VIDEO_FORMAT_xRGB:
    case SPA_VIDEO_FORMAT_ARGB:
        return "0rgb";
    case SPA_VIDEO_FORMAT_BGRx:
    case SPA_VIDEO_FORMAT_BGRA:
    default:
        return "bgr0";
    }
}

static void on_stream_param_changed(void *userdata, uint32_t id, const struct spa_pod *param) {
    CaptureApp *app = (CaptureApp *)userdata;
    if (!param || id != SPA_PARAM_Format) return;
    if (spa_format_parse(param, &app->format.media_type, &app->format.media_subtype) < 0) return;
    if (app->format.media_type != SPA_MEDIA_TYPE_video ||
        app->format.media_subtype != SPA_MEDIA_SUBTYPE_raw)
        return;
    if (spa_format_video_raw_parse(param, &app->format.info.raw) < 0) return;

    uint32_t w = app->format.info.raw.size.width;
    uint32_t h = app->format.info.raw.size.height;
    if (w == 0 || h == 0) return;

    pthread_mutex_lock(&app->frame_lock);
    size_t need = (size_t)w * h * 4;
    if (app->latest_frame_size != need) {
        free(app->latest_frame);
        app->latest_frame = malloc(need);
        app->latest_frame_size = app->latest_frame ? need : 0;
        app->has_first_frame = false;
    }
    app->frame_w = w;
    app->frame_h = h;
    app->pix_fmt = spa_fmt_to_ffmpeg_pix_fmt(app->format.info.raw.format);
    pthread_mutex_unlock(&app->frame_lock);

    uint8_t params_buf[2048];
    struct spa_pod_builder b = SPA_POD_BUILDER_INIT(params_buf, sizeof(params_buf));
    const struct spa_pod *params[2];
    uint32_t n_params = 1;
    params[0] = spa_pod_builder_add_object(
        &b, SPA_TYPE_OBJECT_ParamBuffers, SPA_PARAM_Buffers,
        SPA_PARAM_BUFFERS_buffers, SPA_POD_CHOICE_RANGE_Int(8, 2, 16),
        SPA_PARAM_BUFFERS_blocks, SPA_POD_Int(1),
        SPA_PARAM_BUFFERS_size, SPA_POD_Int((int)(w * h * 4)),
        SPA_PARAM_BUFFERS_stride, SPA_POD_Int((int)(w * 4)),
        SPA_PARAM_BUFFERS_dataType,
        SPA_POD_CHOICE_FLAGS_Int((1 << SPA_DATA_MemFd) | (1 << SPA_DATA_MemPtr) |
                                 (1 << SPA_DATA_DmaBuf)));
    if (app->track) {
        params[n_params++] = spa_pod_builder_add_object(
            &b, SPA_TYPE_OBJECT_ParamMeta, SPA_PARAM_Meta,
            SPA_PARAM_META_type, SPA_POD_Id(SPA_META_Cursor),
            SPA_PARAM_META_size,
            SPA_POD_CHOICE_RANGE_Int(CURSOR_META_SIZE(64, 64), CURSOR_META_SIZE(1, 1),
                                     CURSOR_META_SIZE(CURSOR_MAX, CURSOR_MAX)));
    }
    pw_stream_update_params(app->pw_stream, params, n_params);
}

// Converts one cursor pixel to straight-alpha RGBA. Compositors hand out
// premultiplied cursors, which is detected per image by the caller.
static bool cursor_pixel_layout(uint32_t fmt, int *r, int *g, int *b, int *a) {
    switch (fmt) {
    case SPA_VIDEO_FORMAT_RGBA: *r = 0; *g = 1; *b = 2; *a = 3; return true;
    case SPA_VIDEO_FORMAT_RGBx: *r = 0; *g = 1; *b = 2; *a = -1; return true;
    case SPA_VIDEO_FORMAT_BGRA: *r = 2; *g = 1; *b = 0; *a = 3; return true;
    case SPA_VIDEO_FORMAT_BGRx: *r = 2; *g = 1; *b = 0; *a = -1; return true;
    case SPA_VIDEO_FORMAT_ARGB: *r = 1; *g = 2; *b = 3; *a = 0; return true;
    case SPA_VIDEO_FORMAT_xRGB: *r = 1; *g = 2; *b = 3; *a = -1; return true;
    case SPA_VIDEO_FORMAT_ABGR: *r = 3; *g = 2; *b = 1; *a = 0; return true;
    case SPA_VIDEO_FORMAT_xBGR: *r = 3; *g = 2; *b = 1; *a = -1; return true;
    default: return false;
    }
}

// Reads a new cursor image from the metadata; false when it carries none.
static bool read_cursor_bitmap(CaptureApp *app, const struct spa_meta *meta,
                               const struct spa_meta_cursor *mc, uint32_t *w_out,
                               uint32_t *h_out) {
    if (mc->bitmap_offset < sizeof(*mc) ||
        (size_t)mc->bitmap_offset + sizeof(struct spa_meta_bitmap) > meta->size)
        return false;
    const struct spa_meta_bitmap *bm =
        SPA_PTROFF(mc, mc->bitmap_offset, const struct spa_meta_bitmap);
    uint32_t w = bm->size.width, h = bm->size.height;
    *w_out = w;
    *h_out = h;
    if (w == 0 || h == 0) return true;  // an empty image hides the cursor
    int ri, gi, bi, ai;
    if (w > CURSOR_MAX || h > CURSOR_MAX || !cursor_pixel_layout(bm->format, &ri, &gi, &bi, &ai))
        return false;
    size_t stride = bm->stride > 0 ? (size_t)bm->stride : (size_t)w * 4;
    size_t start = (size_t)mc->bitmap_offset + bm->offset;
    if (bm->offset < sizeof(*bm) || stride < (size_t)w * 4 ||
        start + stride * (h - 1) + (size_t)w * 4 > meta->size)
        return false;
    const uint8_t *px = SPA_PTROFF(mc, start, const uint8_t);
    uint8_t *out = app->shape_tmp;
    bool premultiplied = true;
    for (uint32_t y = 0; y < h; y++) {
        const uint8_t *row = px + y * stride;
        for (uint32_t x = 0; x < w; x++) {
            const uint8_t *p = row + x * 4;
            uint8_t *o = out + ((size_t)y * w + x) * 4;
            o[0] = p[ri];
            o[1] = p[gi];
            o[2] = p[bi];
            o[3] = ai < 0 ? 255 : p[ai];
            if (o[0] > o[3] || o[1] > o[3] || o[2] > o[3]) premultiplied = false;
        }
    }
    if (premultiplied) {
        for (size_t i = 0; i < (size_t)w * h; i++) {
            uint8_t *o = out + i * 4;
            if (o[3] == 0 || o[3] == 255) continue;
            for (int c = 0; c < 3; c++) o[c] = (uint8_t)((o[c] * 255 + o[3] / 2) / o[3]);
        }
    }
    return true;
}

static void track_cursor(CaptureApp *app, struct spa_buffer *buffer) {
    struct spa_meta *meta = spa_buffer_find_meta(buffer, SPA_META_Cursor);
    if (!meta || meta->size < sizeof(struct spa_meta_cursor)) return;
    const struct spa_meta_cursor *mc = meta->data;
    pthread_mutex_lock(&app->track_lock);
    if (!app->track) {
        pthread_mutex_unlock(&app->track_lock);
        return;
    }
    uint32_t id = 0;
    // An invalid cursor is not over this stream (another display, or hidden).
    if (spa_meta_cursor_is_valid(mc)) {
        uint32_t w, h;
        if (read_cursor_bitmap(app, meta, mc, &w, &h)) {
            // An empty image hides the cursor until the next image.
            app->bitmap_hidden = w == 0 || h == 0;
            size_t len = (size_t)w * h * 4;
            bool same = app->bitmap_hidden ||
                        (app->shape && app->shape_w == w && app->shape_h == h &&
                         app->shape_hx == mc->hotspot.x && app->shape_hy == mc->hotspot.y &&
                         memcmp(app->shape, app->shape_tmp, len) == 0);
            uint8_t *copy = same ? NULL : realloc(app->shape, len);
            if (copy) {
                memcpy(copy, app->shape_tmp, len);
                app->shape = copy;
                app->shape_w = w;
                app->shape_h = h;
                app->shape_hx = mc->hotspot.x;
                app->shape_hy = mc->hotspot.y;
                app->shape_id++;
                fprintf(app->track, "S %u %u %u %d %d ", app->shape_id, w, h, app->shape_hx,
                        app->shape_hy);
                for (size_t i = 0; i < len; i++) fprintf(app->track, "%02x", copy[i]);
                fputc('\n', app->track);
            }
        }
        id = app->bitmap_hidden ? 0 : app->shape_id;
    }
    int32_t x = mc->position.x, y = mc->position.y;
    if (id != app->last_id || (id && (x != app->last_x || y != app->last_y))) {
        uint64_t now = monotonic_ns();
        fprintf(app->track, "P %llu %d %d %u\n", (unsigned long long)now, x, y, id);
        app->last_x = x;
        app->last_y = y;
        app->last_id = id;
        __atomic_add_fetch(&app->cursor_updates, 1, __ATOMIC_RELAXED);
        // Flushed about once a second so a crash keeps most of the track.
        if (now - app->track_flushed_ns > 1000000000ULL) {
            fflush(app->track);
            app->track_flushed_ns = now;
        }
    }
    pthread_mutex_unlock(&app->track_lock);
}

static void on_stream_process(void *userdata) {
    CaptureApp *app = (CaptureApp *)userdata;
    struct pw_buffer *b = pw_stream_dequeue_buffer(app->pw_stream);
    if (!b) return;
    if (g_stop_requested) {
        pw_stream_queue_buffer(app->pw_stream, b);
        return;
    }

    if (app->track) track_cursor(app, b->buffer);

    struct spa_data *d = &b->buffer->datas[0];
    uint32_t w = app->format.info.raw.size.width;
    uint32_t h = app->format.info.raw.size.height;
    // Corrupted or cursor-only buffers carry no new image.
    bool has_image = d->chunk && d->chunk->size > 0 &&
                     !(d->chunk->flags & SPA_CHUNK_FLAG_CORRUPTED);

    if (w > 0 && h > 0 && has_image) {
        uint8_t *src = NULL;
        void *mapped = NULL;
        size_t map_len = 0;
        size_t avail = 0;
        if (d->data) {
            src = (uint8_t *)d->data + d->chunk->offset;
            avail = d->maxsize > d->chunk->offset ? d->maxsize - d->chunk->offset : 0;
        } else if ((d->type == SPA_DATA_MemFd || d->type == SPA_DATA_DmaBuf) && d->fd >= 0) {
            map_len = d->maxsize > 0 ? d->maxsize : (size_t)w * h * 4;
            mapped = mmap(NULL, map_len, PROT_READ, MAP_SHARED, (int)d->fd, d->mapoffset);
            if (mapped != MAP_FAILED) {
                src = (uint8_t *)mapped + d->chunk->offset;
                avail = map_len > d->chunk->offset ? map_len - d->chunk->offset : 0;
            }
        }

        size_t row_bytes = (size_t)w * 4;
        size_t stride = d->chunk->stride > 0 ? (size_t)d->chunk->stride : row_bytes;
        // A producer that lies about sizes must not make us read past the buffer.
        if (src && (stride < row_bytes || avail < stride * (h - 1) + row_bytes)) src = NULL;

        if (src) {
            pthread_mutex_lock(&app->frame_lock);
            if (app->latest_frame && app->latest_frame_size == row_bytes * h) {
                if (stride == row_bytes) {
                    memcpy(app->latest_frame, src, row_bytes * h);
                } else {
                    for (uint32_t y = 0; y < h; y++) {
                        memcpy(app->latest_frame + y * row_bytes, src + y * stride, row_bytes);
                    }
                }
                app->has_first_frame = true;
                app->ever_streamed = true;
                app->frame_serial++;
                pthread_cond_signal(&app->frame_ready);
            }
            pthread_mutex_unlock(&app->frame_lock);
        }
        if (mapped && mapped != MAP_FAILED) munmap(mapped, map_len);
    }
    pw_stream_queue_buffer(app->pw_stream, b);
}

static void on_stream_state_changed(void *userdata, enum pw_stream_state old,
                                    enum pw_stream_state state, const char *error) {
    CaptureApp *app = (CaptureApp *)userdata;
    log_err("stream %s -> %s%s%s", pw_stream_state_as_string(old),
            pw_stream_state_as_string(state), error ? ": " : "", error ? error : "");
    pthread_mutex_lock(&app->frame_lock);
    bool streamed = app->ever_streamed;
    pthread_mutex_unlock(&app->frame_lock);

    uint64_t paused_since = 0;
    if (state == PW_STREAM_STATE_PAUSED && streamed) paused_since = monotonic_ns();
    __atomic_store_n(&app->paused_since_ns, paused_since, __ATOMIC_RELAXED);

    if (state == PW_STREAM_STATE_ERROR) {
        if (streamed) {
            char reason[200];
            snprintf(reason, sizeof(reason), "the screen-cast stream failed (%s)",
                     error ? error : "unknown");
            stream_lost(reason);
        } else {
            emit("ERROR PipeWire stream failed (%s)", error ? error : "unknown");
            handle_stop_signal(0);
        }
    } else if (state == PW_STREAM_STATE_UNCONNECTED) {
        // The compositor ended the share (window closed, share revoked).
        if (streamed) {
            stream_lost("the screen-cast stream was disconnected");
        } else {
            handle_stop_signal(0);
        }
    }
}

static void on_registry_global_remove(void *userdata, uint32_t id) {
    CaptureApp *app = (CaptureApp *)userdata;
    if (id == app->pw_node_id) stream_lost("the screen-cast source went away");
}

static const struct pw_registry_events registry_events = {
    PW_VERSION_REGISTRY_EVENTS,
    .global_remove = on_registry_global_remove,
};

static const struct pw_stream_events stream_events = {
    PW_VERSION_STREAM_EVENTS,
    .state_changed = on_stream_state_changed,
    .param_changed = on_stream_param_changed,
    .process = on_stream_process,
};

// ---------------------------------------------------------------------------

static int usage(const char *argv0) {
    fprintf(stderr,
            "Usage: %s --protocol\n"
            "       %s [--token FILE] [--fps N] [--sources monitor|window|both]\n"
            "          [--cursor embedded|hidden|metadata] [--cursor-track FILE]\n"
            "          [--reset] -- <ffmpeg output args>\n",
            argv0, argv0);
    return 1;
}

int main(int argc, char **argv) {
    CaptureApp app;
    memset(&app, 0, sizeof(app));
    app.target_fps = 60;
    app.sources = XDP_OUTPUT_MONITOR;
    app.cursor_mode = XDP_CURSOR_MODE_EMBEDDED;
    app.pw_fd = -1;
    const char *track_path = NULL;

    int i = 1;
    for (; i < argc; i++) {
        const char *a = argv[i];
        const char *v = i + 1 < argc ? argv[i + 1] : NULL;
        if (strcmp(a, "--protocol") == 0) {
            printf("%d\n", HELPER_PROTOCOL);
            return 0;
        } else if (strcmp(a, "--") == 0) {
            i++;
            break;
        } else if (strcmp(a, "--reset") == 0) {
            app.reset_token = true;
        } else if (strcmp(a, "--token") == 0 && v) {
            app.token_path = v;
            i++;
        } else if (strcmp(a, "--fps") == 0 && v) {
            app.target_fps = atoi(v);
            i++;
        } else if (strcmp(a, "--sources") == 0 && v) {
            app.sources = strcmp(v, "window") == 0 ? XDP_OUTPUT_WINDOW
                          : strcmp(v, "both") == 0 ? (XDP_OUTPUT_MONITOR | XDP_OUTPUT_WINDOW)
                                                   : XDP_OUTPUT_MONITOR;
            i++;
        } else if (strcmp(a, "--cursor") == 0 && v) {
            app.cursor_mode = strcmp(v, "hidden") == 0     ? XDP_CURSOR_MODE_HIDDEN
                              : strcmp(v, "metadata") == 0 ? XDP_CURSOR_MODE_METADATA
                                                           : XDP_CURSOR_MODE_EMBEDDED;
            i++;
        } else if (strcmp(a, "--cursor-track") == 0 && v) {
            track_path = v;
            i++;
        } else {
            return usage(argv[0]);
        }
    }
    if (i >= argc) return usage(argv[0]);
    app.ffmpeg_tail = &argv[i];
    app.ffmpeg_tail_len = argc - i;
    if (app.target_fps <= 0 || app.target_fps > 240) app.target_fps = 60;
    pthread_mutex_init(&app.track_lock, NULL);
    if (app.cursor_mode == XDP_CURSOR_MODE_METADATA) {
        if (!track_path) return usage(argv[0]);
        app.shape_tmp = malloc((size_t)CURSOR_MAX * CURSOR_MAX * 4);
        int fd = open(track_path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
        app.track = fd >= 0 ? fdopen(fd, "w") : NULL;
        if (!app.track || !app.shape_tmp) {
            emit("ERROR cannot write the cursor track %s", track_path);
            return EXIT_ENCODER;
        }
        fputs("HGSR-CURSOR 1\n", app.track);
    }

    // A dead FFmpeg must surface as a write error, not kill the helper.
    signal(SIGPIPE, SIG_IGN);
    if (pipe2(g_stop_pipe, O_CLOEXEC | O_NONBLOCK) < 0) g_stop_pipe[0] = g_stop_pipe[1] = -1;
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = handle_stop_signal;
    sigaction(SIGINT, &sa, NULL);
    sigaction(SIGTERM, &sa, NULL);
    pthread_mutex_init(&app.frame_lock, NULL);
    pthread_condattr_t cond_attr;
    pthread_condattr_init(&cond_attr);
    pthread_condattr_setclock(&cond_attr, CLOCK_MONOTONIC);
    pthread_cond_init(&app.frame_ready, &cond_attr);

    app.glib_loop = g_main_loop_new(NULL, FALSE);
    app.portal = xdp_portal_new();
    if (g_stop_pipe[0] >= 0) g_unix_fd_add(g_stop_pipe[0], G_IO_IN, on_stop_during_portal, &app);
    struct stat st;
    if (fstat(STDIN_FILENO, &st) == 0 && S_ISFIFO(st.st_mode))
        g_unix_fd_add(STDIN_FILENO, G_IO_IN | G_IO_HUP | G_IO_ERR, on_stop_during_portal, &app);

    char *saved_tok = app.reset_token ? NULL : load_token(app.token_path);
    xdp_portal_create_screencast_session(app.portal, app.sources, 0, app.cursor_mode,
                                         XDP_PERSIST_MODE_PERSISTENT, saved_tok, NULL,
                                         on_session_created, &app);
    g_main_loop_run(app.glib_loop);
    g_free(saved_tok);

    if (g_stop_requested && !app.portal_ok) {
        if (app.session) xdp_session_close(app.session);
        return 0;
    }
    if (!app.portal_ok) {
        if (app.session) xdp_session_close(app.session);
        return EXIT_PORTAL;
    }

    pw_init(&argc, &argv);
    app.pw_loop = pw_main_loop_new(NULL);
    app.pw_ctx = pw_context_new(pw_main_loop_get_loop(app.pw_loop), NULL, 0);
    app.pw_core = pw_context_connect_fd(app.pw_ctx, app.pw_fd, NULL, 0);
    if (!app.pw_core) {
        emit("ERROR cannot connect to PipeWire");
        return EXIT_PIPEWIRE;
    }
    // Watching the node catches a portal that dies without ending the stream.
    app.pw_registry = pw_core_get_registry(app.pw_core, PW_VERSION_REGISTRY, 0);
    if (app.pw_registry) {
        pw_registry_add_listener(app.pw_registry, &app.registry_listener, &registry_events, &app);
    }
    app.pw_stream = pw_stream_new(app.pw_core, "hypergsr-screencast",
                                  pw_properties_new(PW_KEY_MEDIA_TYPE, "Video",
                                                    PW_KEY_MEDIA_CATEGORY, "Capture",
                                                    PW_KEY_MEDIA_ROLE, "Screen", NULL));
    pw_stream_add_listener(app.pw_stream, &app.stream_listener, &stream_events, &app);

    uint8_t buf[1024];
    struct spa_pod_builder b = SPA_POD_BUILDER_INIT(buf, sizeof(buf));
    const struct spa_pod *params[1];
    params[0] = spa_pod_builder_add_object(
        &b, SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat,
        SPA_FORMAT_mediaType, SPA_POD_Id(SPA_MEDIA_TYPE_video),
        SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
        SPA_FORMAT_VIDEO_format,
        SPA_POD_CHOICE_ENUM_Id(7, SPA_VIDEO_FORMAT_BGRx, SPA_VIDEO_FORMAT_BGRx,
                               SPA_VIDEO_FORMAT_BGRA, SPA_VIDEO_FORMAT_RGBx,
                               SPA_VIDEO_FORMAT_RGBA, SPA_VIDEO_FORMAT_xRGB,
                               SPA_VIDEO_FORMAT_xBGR),
        SPA_FORMAT_VIDEO_size,
        SPA_POD_CHOICE_RANGE_Rectangle(&SPA_RECTANGLE(1920, 1080), &SPA_RECTANGLE(16, 16),
                                       &SPA_RECTANGLE(8192, 8192)),
        SPA_FORMAT_VIDEO_framerate,
        SPA_POD_CHOICE_RANGE_Fraction(&SPA_FRACTION(app.target_fps, 1), &SPA_FRACTION(0, 1),
                                      &SPA_FRACTION(240, 1)));

    if (pw_stream_connect(app.pw_stream, PW_DIRECTION_INPUT, app.pw_node_id,
                          (enum pw_stream_flags)(PW_STREAM_FLAG_AUTOCONNECT |
                                                 PW_STREAM_FLAG_MAP_BUFFERS),
                          params, 1) < 0) {
        emit("ERROR cannot connect to the screen-cast stream");
        return EXIT_PIPEWIRE;
    }

    // The writer thread finalizes the file and exits the process.
    pthread_t writer_tid;
    pthread_create(&writer_tid, NULL, encoder_writer_thread, &app);
    pw_main_loop_run(app.pw_loop);
    pthread_join(writer_tid, NULL);
    return 0;
}
