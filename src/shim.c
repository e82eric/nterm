#include <ghostty.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#ifdef _WIN32
#include <windows.h>
#endif

typedef struct {
    ghostty_app_t app;
    ghostty_surface_t surface;
#ifdef _WIN32
    HWND window;
    WNDPROC previous_window_proc;
#endif
} GhosttyEguiHost;

#ifdef _WIN32
static const wchar_t host_property[] = L"GhosttyEguiHost.QuickSelect";
static LRESULT CALLBACK host_window_proc(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
    GhosttyEguiHost *host = (GhosttyEguiHost *)GetPropW(window, host_property);
    if (!host) return DefWindowProcW(window, message, wparam, lparam);
    // Winit still receives WM_SYSKEYDOWN. Only prevent the default system-menu
    // action generated for Alt+Space, so the app can own this shortcut.
    if (message == WM_SYSCOMMAND && (wparam & 0xfff0) == SC_KEYMENU && lparam == L' ') return 0;
    WNDPROC previous = host->previous_window_proc;
    if (message == WM_NCDESTROY) {
        SetWindowLongPtrW(window, GWLP_WNDPROC, (LONG_PTR)previous);
        RemovePropW(window, host_property);
        host->window = NULL;
    }
    return CallWindowProcW(previous, window, message, wparam, lparam);
}
#endif

extern void ghostty_egui_wakeup(void *userdata);
extern void ghostty_egui_frame_ready(void *userdata);
extern void ghostty_egui_close(void *userdata);

static void wake(void *userdata) { ghostty_egui_wakeup(userdata); }
static bool action(ghostty_app_t app, ghostty_target_s target, ghostty_action_s action_value) {
    if (target.tag != GHOSTTY_TARGET_SURFACE) return false;

    void *userdata = ghostty_app_userdata(app);
    switch (action_value.tag) {
    case GHOSTTY_ACTION_RENDER:
        ghostty_egui_frame_ready(userdata);
        return true;
    default:
        return false;
    }
}
static ghostty_clipboard_read_result_e read_clipboard(
    void *userdata,
    ghostty_clipboard_e clipboard,
    void *state,
    const char *const *types,
    size_t count,
    bool confirm
) {
    (void)userdata;
    (void)clipboard;
    (void)state;
    (void)types;
    (void)count;
    (void)confirm;
    return GHOSTTY_CLIPBOARD_READ_UNSUPPORTED;
}
static void confirm_clipboard(
    void *userdata,
    const ghostty_clipboard_confirm_s *value,
    void *state,
    ghostty_clipboard_request_e request
) {
    (void)userdata;
    (void)value;
    (void)state;
    (void)request;
}
static void write_clipboard(
    void *userdata,
    ghostty_clipboard_e clipboard,
    const ghostty_clipboard_content_s *contents,
    size_t count,
    bool confirm
) {
    (void)userdata;
    (void)clipboard;
    (void)contents;
    (void)count;
    (void)confirm;
}
static void close_surface(void *userdata, bool process_alive) {
    (void)process_alive;
    ghostty_egui_close(userdata);
}

GhosttyEguiHost *ghostty_egui_host_create(
    size_t argc,
    char **argv,
    void *userdata,
    void *window,
    void *display,
    void *context,
    double scale
) {
    if (ghostty_init(argc, argv) != 0) return NULL;

    ghostty_config_t config = ghostty_config_new();
    if (!config) return NULL;
    ghostty_config_load_default_files(config);
    ghostty_config_load_cli_args(config);
    ghostty_config_load_recursive_files(config);
    ghostty_config_finalize(config);

    ghostty_runtime_config_s runtime = {
        .userdata = userdata,
        .wakeup_cb = wake,
        .action_cb = action,
        .read_clipboard_cb = read_clipboard,
        .confirm_read_clipboard_cb = confirm_clipboard,
        .write_clipboard_cb = write_clipboard,
        .close_surface_cb = close_surface,
    };
    ghostty_app_t app = ghostty_app_new(&runtime, config);
    ghostty_config_free(config);
    if (!app) return NULL;

    ghostty_surface_config_s options = ghostty_surface_config_new();
    options.platform_tag = GHOSTTY_PLATFORM_OPENGL;
    options.platform.opengl.window = window;
    options.platform.opengl.display = display;
    options.platform.opengl.context = context;
    options.scale_factor = scale;
    ghostty_surface_t surface = ghostty_surface_new(app, &options);
    if (!surface) {
        ghostty_app_free(app);
        return NULL;
    }

    GhosttyEguiHost *host = calloc(1, sizeof(*host));
    if (!host) {
        ghostty_surface_free(surface);
        ghostty_app_free(app);
        return NULL;
    }
    host->app = app;
    host->surface = surface;
#ifdef _WIN32
    if (SetPropW((HWND)window, host_property, host)) {
        host->previous_window_proc = (WNDPROC)SetWindowLongPtrW((HWND)window, GWLP_WNDPROC, (LONG_PTR)host_window_proc);
        if (host->previous_window_proc) host->window = (HWND)window;
        else RemovePropW((HWND)window, host_property);
    }
#endif
    return host;
}

void ghostty_egui_host_destroy(GhosttyEguiHost *host) {
    if (!host) return;
#ifdef _WIN32
    if (host->window) {
        if ((WNDPROC)GetWindowLongPtrW(host->window, GWLP_WNDPROC) == host_window_proc)
            SetWindowLongPtrW(host->window, GWLP_WNDPROC, (LONG_PTR)host->previous_window_proc);
        RemovePropW(host->window, host_property);
    }
#endif
    if (host->surface) ghostty_surface_free(host->surface);
    if (host->app) ghostty_app_free(host->app);
    free(host);
}

bool ghostty_egui_host_read_font(GhosttyEguiHost *host, bool bold, ghostty_font_data_s *result) {
    return ghostty_surface_read_font(host->surface, bold, result);
}
ghostty_surface_size_s ghostty_egui_host_surface_size(GhosttyEguiHost *host) {
    return ghostty_surface_size(host->surface);
}
void ghostty_egui_host_tick(GhosttyEguiHost *host) { ghostty_app_tick(host->app); }
void ghostty_egui_host_resize(GhosttyEguiHost *host, uint32_t width, uint32_t height, double scale) {
    ghostty_surface_set_content_scale(host->surface, scale, scale);
    ghostty_surface_set_size(host->surface, width, height);
}
void ghostty_egui_host_focus(GhosttyEguiHost *host, bool focused) {
    ghostty_app_set_focus(host->app, focused);
    ghostty_surface_set_focus(host->surface, focused);
}
bool ghostty_egui_host_take_frame(GhosttyEguiHost *host, ghostty_gl_frame_s *frame) {
    return ghostty_surface_take_gl_frame(host->surface, frame);
}
void ghostty_egui_host_release_frame(ghostty_gl_frame_t frame) { ghostty_gl_frame_release(frame); }
bool ghostty_egui_host_key(
    GhosttyEguiHost *host,
    int action_value,
    int mods,
    uint32_t usb,
    uint32_t unshifted,
    const char *text
) {
    return ghostty_surface_key_usb(host->surface, action_value, mods, usb, unshifted, text);
}
bool ghostty_egui_host_snapshot_begin(GhosttyEguiHost *host, uint64_t *snapshot_generation) {
    return ghostty_surface_snapshot_begin(host->surface, snapshot_generation);
}
bool ghostty_egui_host_snapshot_end(GhosttyEguiHost *host, uint64_t snapshot_generation) {
    return ghostty_surface_snapshot_end(host->surface, snapshot_generation);
}
bool ghostty_egui_host_snapshot_view(
    GhosttyEguiHost *host, uint64_t generation, ghostty_snapshot_view_s *view
) { return ghostty_surface_snapshot_view(host->surface, generation, view); }
bool ghostty_egui_host_snapshot_resolve_cell(
    GhosttyEguiHost *host, uint64_t generation, ghostty_snapshot_cell_s requested,
    bool snap_right, ghostty_snapshot_cell_s *actual
) { return ghostty_surface_snapshot_resolve_cell(
    host->surface, generation, requested, snap_right, actual
); }
bool ghostty_egui_host_snapshot_selection_text(
    GhosttyEguiHost *host, uint64_t generation,
    ghostty_snapshot_cell_range_s range, bool trim, bool rectangle,
    ghostty_snapshot_selection_text_s *result
) { return ghostty_surface_snapshot_selection_text(host->surface, generation, range, trim, rectangle, result); }
void ghostty_egui_host_snapshot_selection_text_free(
    ghostty_snapshot_selection_text_s *result
) { ghostty_surface_snapshot_selection_text_free(result); }
ghostty_snapshot_text_cursor_t ghostty_egui_host_snapshot_cursor_new(
    GhosttyEguiHost *host, uint64_t generation
) { return ghostty_surface_snapshot_text_cursor_new(host->surface, generation); }
void ghostty_egui_host_snapshot_cursor_free(ghostty_snapshot_text_cursor_t cursor) {
    ghostty_surface_snapshot_text_cursor_free(cursor);
}
bool ghostty_egui_host_snapshot_cursor_reset(ghostty_snapshot_text_cursor_t cursor) {
    return ghostty_surface_snapshot_text_cursor_reset(cursor);
}
int ghostty_egui_host_snapshot_cursor_next(
    ghostty_snapshot_text_cursor_t cursor, ghostty_snapshot_text_chunk_s *chunk
) { return ghostty_surface_snapshot_text_cursor_next(cursor, chunk); }
bool ghostty_egui_host_snapshot_text_row(
    GhosttyEguiHost *host, uint64_t generation, uint64_t row,
    ghostty_snapshot_text_row_s *result
) { return ghostty_surface_snapshot_text_row(host->surface, generation, row, result); }
void ghostty_egui_host_snapshot_text_row_free(ghostty_snapshot_text_row_s *result) {
    ghostty_surface_snapshot_text_row_free(result);
}
bool ghostty_egui_host_snapshot_apply_decorations(
    GhosttyEguiHost *host, uint64_t generation,
    const ghostty_snapshot_decoration_layer_s *layers, size_t count,
    const ghostty_snapshot_viewport_options_s *viewport
) { return ghostty_surface_snapshot_apply_decorations(
    host->surface, generation, layers, count, NULL, 0, viewport
); }
bool ghostty_egui_host_snapshot_apply_cell_text(
    GhosttyEguiHost *host, uint64_t generation,
    const ghostty_snapshot_decoration_layer_s *layers, size_t count,
    const ghostty_snapshot_cell_text_s *text, size_t text_count,
    const ghostty_snapshot_viewport_options_s *viewport
) { return ghostty_surface_snapshot_apply_decorations(
    host->surface, generation, layers, count, text, text_count, viewport
); }
bool ghostty_egui_host_snapshot_decorations_need_refresh(
    GhosttyEguiHost *host, uint64_t generation
) { return ghostty_surface_snapshot_decorations_need_refresh(host->surface, generation); }
void ghostty_egui_host_text(GhosttyEguiHost *host, const char *text, size_t len) {
    ghostty_surface_text(host->surface, text, len);
}
void ghostty_egui_host_preedit(GhosttyEguiHost *host, const char *text, size_t len) {
    ghostty_surface_preedit(host->surface, text, len);
}
void ghostty_egui_host_mouse_pos(GhosttyEguiHost *host, double x, double y, int mods) {
    ghostty_surface_mouse_pos(host->surface, x, y, (ghostty_input_mods_e)mods);
}
void ghostty_egui_host_mouse_button(GhosttyEguiHost *host, int state, int button, int mods) {
    ghostty_surface_mouse_button(host->surface,
        (ghostty_input_mouse_state_e)state,
        (ghostty_input_mouse_button_e)button,
        (ghostty_input_mods_e)mods);
}
void ghostty_egui_host_mouse_scroll(GhosttyEguiHost *host, double x, double y, int mods) {
    ghostty_surface_mouse_scroll(host->surface, x, y, mods);
}
