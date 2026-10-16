// Native XDG Desktop Portal ScreenCast + PipeWire -> NVENC MP4 Recorder Engine
// Part of HyperGSR (gpu-recorder-rs)
// Compiles against system libportal.so.1, libpipewire-0.3.so.0, and gio-2.0.

#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <stdbool.h>
#include <string.h>
#include <unistd.h>
#include <signal.h>
#include <fcntl.h>
#include <time.h>
#include <poll.h>
#include <pthread.h>
#include <sys/stat.h>
#include <sys/mman.h>
#include <sys/wait.h>

#include <glib.h>
#include <gio/gio.h>

#include <pipewire/pipewire.h>
#include <spa/param/video/format-utils.h>
#include <spa/param/buffers.h>
#include <spa/debug/types.h>

// Forward declarations for libportal.so.1 ABI
typedef struct _XdpPortal XdpPortal;
typedef struct _XdpSession XdpSession;
typedef struct _XdpParent XdpParent;

extern XdpPortal *xdp_portal_new(void);
extern void xdp_portal_create_screencast_session(
    XdpPortal *portal,
    int outputs,
    int flags,
    int cursor_mode,
    int persist_mode,
    const char *restore_token,
    GCancellable *cancellable,
    GAsyncReadyCallback callback,
    gpointer data
);
extern XdpSession *xdp_portal_create_screencast_session_finish(
    XdpPortal *portal,
    GAsyncResult *result,
    GError **error
);
extern void xdp_session_start(
    XdpSession *session,
    XdpParent *parent,
    GCancellable *cancellable,
    GAsyncReadyCallback callback,
    gpointer data
);
extern gboolean xdp_session_start_finish(
    XdpSession *session,
    GAsyncResult *result,
    GError **error
);
extern const char *xdp_session_get_restore_token(XdpSession *session);
extern GVariant *xdp_session_get_streams(XdpSession *session);
extern int xdp_session_open_pipewire_remote(XdpSession *session);
extern void xdp_session_close(XdpSession *session);

static volatile sig_atomic_t g_stop_requested = 0;
static int g_stop_pipe[2] = {-1, -1};

static void handle_sig(int sig) {
    (void)sig;
    g_stop_requested = 1;
    if (g_stop_pipe[1] >= 0) {
        const char ch = 'q';
        ssize_t _r = write(g_stop_pipe[1], &ch, 1);
        (void)_r;
    }
}

typedef struct {
    GMainLoop *glib_loop;
    XdpPortal *portal;
    XdpSession *session;
    const char *token_path;
    const char *output_path;
    int target_fps;
    uint32_t pw_node_id;
    int pw_fd;
    bool ok;

    struct pw_main_loop *pw_loop;
    struct pw_context *pw_ctx;
    struct pw_core *pw_core;
    struct pw_stream *pw_stream;
    struct spa_hook stream_listener;
    struct spa_video_info format;
    bool format_ready;

    pthread_mutex_t frame_lock;
    uint8_t *latest_frame;
    size_t latest_frame_size;
    uint32_t frame_w;
    uint32_t frame_h;
    const char *pix_fmt;
    bool has_first_frame;

    FILE *ffmpeg_pipe;
    uint64_t frames_written;
} CaptureApp;

static uint64_t monotonic_ns(void);
static FILE *spawn_ffmpeg_encoder(const char *output_path, uint32_t width, uint32_t height, int fps, const char *pix_fmt);

static void *encoder_writer_thread(void *arg) {
    CaptureApp *app = (CaptureApp *)arg;
    struct pollfd fds[2];
    int nfds = 0;
    if (g_stop_pipe[0] >= 0) {
        fds[nfds].fd = g_stop_pipe[0];
        fds[nfds].events = POLLIN | POLLHUP | POLLERR;
        nfds++;
    }
    struct stat st;
    if (fstat(STDIN_FILENO, &st) == 0 && S_ISFIFO(st.st_mode)) {
        fds[nfds].fd = STDIN_FILENO;
        fds[nfds].events = POLLIN | POLLHUP | POLLERR;
        nfds++;
    }

    uint64_t interval_ns = 1000000000ULL / (uint64_t)(app->target_fps > 0 ? app->target_fps : 60);
    uint64_t next_tick_ns = 0;
    uint8_t *write_buf = NULL;
    size_t write_buf_size = 0;

    while (!g_stop_requested) {
        int r = poll(fds, nfds, 0);
        if (r > 0) {
            for (int i = 0; i < nfds; i++) {
                if (fds[i].revents & (POLLIN | POLLHUP | POLLERR)) {
                    g_stop_requested = 1;
                    break;
                }
            }
            if (g_stop_requested) break;
        }

        uint64_t now_ns = monotonic_ns();
        if (next_tick_ns == 0) {
            next_tick_ns = now_ns;
        }
        if (now_ns < next_tick_ns) {
            uint64_t wait_ns = next_tick_ns - now_ns;
            if (wait_ns > 2000000ULL) wait_ns = 2000000ULL;
            struct timespec req = {0, (long)wait_ns};
            nanosleep(&req, NULL);
            continue;
        }

        pthread_mutex_lock(&app->frame_lock);
        bool ready = app->has_first_frame && app->latest_frame && app->latest_frame_size > 0;
        uint32_t w = app->frame_w;
        uint32_t h = app->frame_h;
        const char *pix_fmt = app->pix_fmt;
        if (ready) {
            if (write_buf_size != app->latest_frame_size) {
                free(write_buf);
                write_buf_size = app->latest_frame_size;
                write_buf = (uint8_t *)malloc(write_buf_size);
            }
            if (write_buf) {
                memcpy(write_buf, app->latest_frame, write_buf_size);
            }
        }
        pthread_mutex_unlock(&app->frame_lock);

        if (ready && write_buf) {
            if (!app->ffmpeg_pipe) {
                app->ffmpeg_pipe = spawn_ffmpeg_encoder(app->output_path, w, h, app->target_fps, pix_fmt);
                next_tick_ns = monotonic_ns();
            }
            if (app->ffmpeg_pipe) {
                fwrite(write_buf, 1, write_buf_size, app->ffmpeg_pipe);
                app->frames_written++;
            }
        }

        next_tick_ns += interval_ns;
        if (now_ns > next_tick_ns + interval_ns * 4) {
            next_tick_ns = now_ns;
        }
    }

    free(write_buf);
    if (app->ffmpeg_pipe) {
        fflush(app->ffmpeg_pipe);
        pclose(app->ffmpeg_pipe);
        app->ffmpeg_pipe = NULL;
    }
    _exit(0);
    return NULL;
}

static uint64_t monotonic_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

static void save_token(const char *path, const char *token) {
    if (!path || !token || !token[0]) return;
    FILE *f = fopen(path, "w");
    if (f) {
        fprintf(f, "%s", token);
        fclose(f);
    }
}

static char *load_token(const char *path) {
    if (!path) return NULL;
    FILE *f = fopen(path, "r");
    if (!f) return NULL;
    char buf[256] = {0};
    if (!fgets(buf, sizeof(buf) - 1, f)) {
        fclose(f);
        return NULL;
    }
    fclose(f);
    size_t len = strlen(buf);
    while (len > 0 && (buf[len - 1] == '\n' || buf[len - 1] == '\r' || buf[len - 1] == ' ')) {
        buf[--len] = '\0';
    }
    return len > 0 ? g_strdup(buf) : NULL;
}

static void on_session_started(GObject *source, GAsyncResult *res, gpointer user_data) {
    CaptureApp *app = (CaptureApp *)user_data;
    GError *err = NULL;
    if (!xdp_session_start_finish(app->session, res, &err)) {
        fprintf(stderr, "hypergsr-pw-capture: xdp_session_start failed: %s\n",
                err ? err->message : "unknown");
        if (err) g_error_free(err);
        g_main_loop_quit(app->glib_loop);
        return;
    }

    const char *new_tok = xdp_session_get_restore_token(app->session);
    if (new_tok) {
        save_token(app->token_path, new_tok);
    }

    GVariant *streams = xdp_session_get_streams(app->session);
    if (!streams) {
        fprintf(stderr, "hypergsr-pw-capture: no streams in screencast session\n");
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
        fprintf(stderr, "hypergsr-pw-capture: stream node_id is 0\n");
        g_main_loop_quit(app->glib_loop);
        return;
    }

    app->pw_fd = xdp_session_open_pipewire_remote(app->session);
    if (app->pw_fd < 0) {
        fprintf(stderr, "hypergsr-pw-capture: failed to open PipeWire remote FD\n");
        g_main_loop_quit(app->glib_loop);
        return;
    }

    app->ok = true;
    g_main_loop_quit(app->glib_loop);
}

static void on_session_created(GObject *source, GAsyncResult *res, gpointer user_data) {
    (void)source;
    CaptureApp *app = (CaptureApp *)user_data;
    GError *err = NULL;
    app->session = xdp_portal_create_screencast_session_finish(app->portal, res, &err);
    if (!app->session) {
        fprintf(stderr, "hypergsr-pw-capture: create_screencast_session failed: %s\n",
                err ? err->message : "unknown");
        if (err) g_error_free(err);
        g_main_loop_quit(app->glib_loop);
        return;
    }

    xdp_session_start(app->session, NULL, NULL, on_session_started, app);
}

static const char *spa_fmt_to_ffmpeg_pix_fmt(uint32_t fmt) {
    switch (fmt) {
        case SPA_VIDEO_FORMAT_BGRx:
        case SPA_VIDEO_FORMAT_BGRA:
            return "bgr0";
        case SPA_VIDEO_FORMAT_RGBx:
        case SPA_VIDEO_FORMAT_RGBA:
            return "rgb0";
        case SPA_VIDEO_FORMAT_xBGR:
        case SPA_VIDEO_FORMAT_ABGR:
            return "0bgr";
        case SPA_VIDEO_FORMAT_xRGB:
        case SPA_VIDEO_FORMAT_ARGB:
            return "0rgb";
        default:
            return "bgr0";
    }
}

static FILE *spawn_ffmpeg_encoder(const char *output_path, uint32_t width, uint32_t height, int fps, const char *pix_fmt) {
    char cmd[2048];
    snprintf(
        cmd,
        sizeof(cmd),
        "trap '' INT TERM; "
        "ffmpeg -y -hide_banner -loglevel error "
        "-f rawvideo -pix_fmt %s -video_size %ux%u -framerate %d -i pipe:0 "
        "-c:v h264_nvenc -preset p4 -tune hq -rc vbr -cq 19 -b:v 0 -pix_fmt yuv420p -movflags +faststart \"%s\" "
        "|| ffmpeg -y -hide_banner -loglevel error "
        "-f rawvideo -pix_fmt %s -video_size %ux%u -framerate %d -i pipe:0 "
        "-c:v libx264 -preset veryfast -crf 19 -pix_fmt yuv420p -movflags +faststart \"%s\"",
        pix_fmt, width, height, fps, output_path,
        pix_fmt, width, height, fps, output_path
    );
    return popen(cmd, "w");
}

static void on_stream_param_changed(void *userdata, uint32_t id, const struct spa_pod *param) {
    CaptureApp *app = (CaptureApp *)userdata;
    if (!param || id != SPA_PARAM_Format) return;

    if (spa_format_parse(param, &app->format.media_type, &app->format.media_subtype) < 0)
        return;
    if (app->format.media_type != SPA_MEDIA_TYPE_video ||
        app->format.media_subtype != SPA_MEDIA_SUBTYPE_raw)
        return;
    if (spa_format_video_raw_parse(param, &app->format.info.raw) < 0)
        return;

    uint32_t w = app->format.info.raw.size.width;
    uint32_t h = app->format.info.raw.size.height;
    const char *pix_fmt = spa_fmt_to_ffmpeg_pix_fmt(app->format.info.raw.format);
    pthread_mutex_lock(&app->frame_lock);
    if (w > 0 && h > 0) {
        size_t need = (size_t)w * h * 4;
        if (app->latest_frame_size != need) {
            free(app->latest_frame);
            app->latest_frame = (uint8_t *)malloc(need);
            app->latest_frame_size = app->latest_frame ? need : 0;
        }
        app->frame_w = w;
        app->frame_h = h;
        app->pix_fmt = pix_fmt;
        app->format_ready = (app->latest_frame != NULL);
    }
    pthread_mutex_unlock(&app->frame_lock);

    uint8_t params_buf[1024];
    struct spa_pod_builder b = SPA_POD_BUILDER_INIT(params_buf, sizeof(params_buf));
    const struct spa_pod *params[1];
    params[0] = spa_pod_builder_add_object(
        &b,
        SPA_TYPE_OBJECT_ParamBuffers, SPA_PARAM_Buffers,
        SPA_PARAM_BUFFERS_buffers, SPA_POD_CHOICE_RANGE_Int(8, 2, 16),
        SPA_PARAM_BUFFERS_blocks, SPA_POD_Int(1),
        SPA_PARAM_BUFFERS_size, SPA_POD_Int((int)(w * h * 4)),
        SPA_PARAM_BUFFERS_stride, SPA_POD_Int((int)(w * 4)),
        SPA_PARAM_BUFFERS_dataType, SPA_POD_CHOICE_FLAGS_Int((1 << SPA_DATA_MemFd) | (1 << SPA_DATA_MemPtr) | (1 << SPA_DATA_DmaBuf))
    );
    pw_stream_update_params(app->pw_stream, params, 1);
}

static void on_stream_process(void *userdata) {
    CaptureApp *app = (CaptureApp *)userdata;
    if (g_stop_requested) {
        return;
    }

    struct pw_buffer *b = pw_stream_dequeue_buffer(app->pw_stream);
    if (!b) return;

    struct spa_buffer *buf = b->buffer;
    struct spa_data *d = &buf->datas[0];
    uint32_t w = app->format.info.raw.size.width;
    uint32_t h = app->format.info.raw.size.height;

    if (!g_stop_requested && w > 0 && h > 0 && d->chunk && d->chunk->size > 0) {
        uint8_t *src = NULL;
        void *mapped = NULL;
        size_t map_len = 0;

        if (d->data != NULL) {
            src = (uint8_t *)d->data + d->chunk->offset;
        } else if ((d->type == SPA_DATA_MemFd || d->type == SPA_DATA_DmaBuf) && d->fd >= 0) {
            map_len = d->maxsize > 0 ? d->maxsize : (size_t)(w * h * 4);
            mapped = mmap(NULL, map_len, PROT_READ, MAP_SHARED, (int)d->fd, d->mapoffset);
            if (mapped != MAP_FAILED) {
                src = (uint8_t *)mapped + d->chunk->offset;
            }
        }

        if (src) {
            uint32_t row_bytes = w * 4;
            uint32_t stride = d->chunk->stride > 0 ? (uint32_t)d->chunk->stride : row_bytes;
            pthread_mutex_lock(&app->frame_lock);
            if (app->latest_frame && app->latest_frame_size == (size_t)row_bytes * h) {
                if (stride == row_bytes) {
                    memcpy(app->latest_frame, src, (size_t)row_bytes * h);
                } else {
                    for (uint32_t y = 0; y < h; y++) {
                        memcpy(app->latest_frame + (size_t)y * row_bytes, src + (size_t)y * stride, row_bytes);
                    }
                }
                app->has_first_frame = true;
            }
            pthread_mutex_unlock(&app->frame_lock);
        }

        if (mapped && mapped != MAP_FAILED) {
            munmap(mapped, map_len);
        }
    }

    pw_stream_queue_buffer(app->pw_stream, b);
}

static const struct pw_stream_events stream_events = {
    PW_VERSION_STREAM_EVENTS,
    .param_changed = on_stream_param_changed,
    .process = on_stream_process,
};

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "Usage: %s <output.mp4> <token_file> [fps]\n", argv[0]);
        return 1;
    }

    if (pipe2(g_stop_pipe, O_CLOEXEC | O_NONBLOCK) < 0) {
        g_stop_pipe[0] = g_stop_pipe[1] = -1;
    }
    signal(SIGINT, handle_sig);
    signal(SIGTERM, handle_sig);

    CaptureApp app;
    memset(&app, 0, sizeof(app));
    pthread_mutex_init(&app.frame_lock, NULL);
    app.output_path = argv[1];
    app.token_path = argv[2];
    app.target_fps = (argc >= 4) ? atoi(argv[3]) : 60;
    if (app.target_fps <= 0) app.target_fps = 60;
    app.pw_fd = -1;

    app.glib_loop = g_main_loop_new(NULL, FALSE);
    app.portal = xdp_portal_new();

    char *saved_tok = load_token(app.token_path);
    xdp_portal_create_screencast_session(
        app.portal,
        1, // XDP_OUTPUT_MONITOR
        0, // XDP_SCREENCAST_FLAG_NONE
        2, // XDP_CURSOR_MODE_EMBEDDED
        2, // XDP_PERSIST_MODE_PERSISTENT
        saved_tok,
        NULL,
        on_session_created,
        &app
    );
    g_main_loop_run(app.glib_loop);
    if (saved_tok) g_free(saved_tok);

    if (!app.ok || app.pw_fd < 0 || app.pw_node_id == 0) {
        if (app.session) xdp_session_close(app.session);
        g_main_loop_unref(app.glib_loop);
        return 2;
    }

    pthread_t writer_tid;
    pthread_create(&writer_tid, NULL, encoder_writer_thread, &app);
    pthread_detach(writer_tid);

    pw_init(&argc, &argv);
    app.pw_loop = pw_main_loop_new(NULL);
    app.pw_ctx = pw_context_new(pw_main_loop_get_loop(app.pw_loop), NULL, 0);
    app.pw_core = pw_context_connect_fd(app.pw_ctx, app.pw_fd, NULL, 0);
    if (!app.pw_core) {
        fprintf(stderr, "hypergsr-pw-capture: pw_context_connect_fd failed\n");
        return 3;
    }

    app.pw_stream = pw_stream_new(
        app.pw_core,
        "hypergsr-screencast",
        pw_properties_new(
            PW_KEY_MEDIA_TYPE, "Video",
            PW_KEY_MEDIA_CATEGORY, "Capture",
            PW_KEY_MEDIA_ROLE, "Screen",
            NULL
        )
    );
    pw_stream_add_listener(app.pw_stream, &app.stream_listener, &stream_events, &app);

    uint8_t buf[1024];
    struct spa_pod_builder b = SPA_POD_BUILDER_INIT(buf, sizeof(buf));
    const struct spa_pod *params[1];
    params[0] = spa_pod_builder_add_object(
        &b,
        SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat,
        SPA_FORMAT_mediaType, SPA_POD_Id(SPA_MEDIA_TYPE_video),
        SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
        SPA_FORMAT_VIDEO_format, SPA_POD_CHOICE_ENUM_Id(
            6,
            SPA_VIDEO_FORMAT_BGRx,
            SPA_VIDEO_FORMAT_BGRx,
            SPA_VIDEO_FORMAT_BGRA,
            SPA_VIDEO_FORMAT_RGBx,
            SPA_VIDEO_FORMAT_RGBA,
            SPA_VIDEO_FORMAT_xRGB
        ),
        SPA_FORMAT_VIDEO_size, SPA_POD_CHOICE_RANGE_Rectangle(
            &SPA_RECTANGLE(1920, 1080),
            &SPA_RECTANGLE(320, 240),
            &SPA_RECTANGLE(3840, 2160)
        ),
        SPA_FORMAT_VIDEO_framerate, SPA_POD_CHOICE_RANGE_Fraction(
            &SPA_FRACTION(app.target_fps, 1),
            &SPA_FRACTION(1, 1),
            &SPA_FRACTION(120, 1)
        )
    );

    pw_stream_connect(
        app.pw_stream,
        PW_DIRECTION_INPUT,
        app.pw_node_id,
        (enum pw_stream_flags)(PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS),
        params,
        1
    );

    pw_main_loop_run(app.pw_loop);
    _exit(0);
}
