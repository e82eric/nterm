#include <ghostty.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <errno.h>
#ifdef _WIN32
#include <windows.h>
#endif

typedef struct GhosttyEguiHost GhosttyEguiHost;
typedef struct {
    ghostty_config_t base_config;
    GhosttyEguiHost *hosts;
} GhosttyEguiShared;

struct GhosttyEguiHost {
    ghostty_app_t app;
    ghostty_surface_t surface;
    GhosttyEguiShared *shared;
    GhosttyEguiHost *next;
    bool focused;
    ghostty_platform_opengl_s platform;
#ifdef _WIN32
    HWND window;
    WNDPROC previous_window_proc;
#endif
};

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

extern void ghostty_egui_frame_ready(void *userdata);
extern void ghostty_egui_close(void *userdata);
extern void ghostty_egui_write_clipboard(void *userdata, const uint8_t *bytes, size_t len);
extern void ghostty_egui_config_issue(void *userdata, const char *message);
extern void ghostty_egui_runtime_issue(void *userdata, const char *operation, const char *cause);

static void runtime_error(GhosttyEguiHost *host, const char *operation) {
    const char *cause = ghostty_last_error();
    if (cause && host && host->surface)
        ghostty_egui_runtime_issue(ghostty_surface_userdata(host->surface), operation, cause);
}

typedef struct {
    uint32_t background, foreground, selection_background, selection_foreground;
    uint32_t selection_flags;
    uint32_t ansi[16];
} GhosttyEguiThemeColors;
extern void ghostty_egui_theme_colors(void *userdata, const GhosttyEguiThemeColors *colors, const char *name, size_t name_len);

static uint32_t pack_color(ghostty_config_color_s color) {
    return ((uint32_t)color.r << 16) | ((uint32_t)color.g << 8) | color.b;
}

static void report_theme_colors(ghostty_config_t config, void *userdata) {
    GhosttyEguiThemeColors colors = {0};
    ghostty_config_color_s color;
    ghostty_config_palette_s palette;
    if (!ghostty_config_get(config, &color, "background", strlen("background"))) return;
    colors.background = pack_color(color);
    if (!ghostty_config_get(config, &color, "foreground", strlen("foreground"))) return;
    colors.foreground = pack_color(color);
    if (!ghostty_config_get(config, &palette, "palette", strlen("palette"))) return;
    for (size_t i = 0; i < 16; ++i) colors.ansi[i] = pack_color(palette.colors[i]);
    if (ghostty_config_get(config, &color, "selection-background", strlen("selection-background"))) {
        colors.selection_background = pack_color(color);
        colors.selection_flags |= 1;
    }
    if (ghostty_config_get(config, &color, "selection-foreground", strlen("selection-foreground"))) {
        colors.selection_foreground = pack_color(color);
        colors.selection_flags |= 2;
    }
    ghostty_config_theme_name_s name = {0};
    ghostty_config_get(config, &name, "theme", strlen("theme"));
    ghostty_egui_theme_colors(userdata, &colors, name.ptr, name.len);
}

extern void ghostty_egui_title(void *userdata, const char *title);
extern void ghostty_egui_pwd(void *userdata, const char *pwd);
extern void ghostty_egui_app_wakeup(void);
static void wake(void *userdata) { (void)userdata; ghostty_egui_app_wakeup(); }
static bool action(ghostty_app_t app, ghostty_target_s target, ghostty_action_s action_value) {
    if (target.tag != GHOSTTY_TARGET_SURFACE) return false;

    void *userdata = ghostty_surface_userdata(target.target.surface);
    switch (action_value.tag) {
    case GHOSTTY_ACTION_SET_TITLE:
        ghostty_egui_title(userdata, action_value.action.set_title.title);
        return true;
    case GHOSTTY_ACTION_PWD:
        // Ghostty validates OSC 7 (local host only) and decodes the path.
        ghostty_egui_pwd(userdata, action_value.action.pwd.pwd);
        return true;
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
    if (clipboard != GHOSTTY_CLIPBOARD_STANDARD || confirm) return;
    for (size_t i = 0; i < count; ++i) {
        if (contents[i].mime && strncmp(contents[i].mime, "text/plain", 10) == 0 &&
            contents[i].data) {
            ghostty_egui_write_clipboard(userdata, (const uint8_t *)contents[i].data, contents[i].len);
            return;
        }
    }
}
static void close_surface(void *userdata, bool process_alive) {
    (void)process_alive;
    ghostty_egui_close(userdata);
}

static void startup_error(char *buffer, size_t capacity, const char *stage, const char *cause) {
    if (buffer && capacity) snprintf(buffer, capacity, "Failed to %s.\n\nNative error: %s", stage,
        cause ? cause : "The native API returned failure without additional details.");
}

GhosttyEguiHost *ghostty_egui_host_create(
    size_t argc,
    char **argv,
    void *userdata,
    void *window,
    void *display,
    void *context,
    double scale,
    const char *picker_endpoint,
    const char *picker_token,
    char *error_buffer,
    size_t error_capacity
) {
    if (error_buffer && error_capacity) error_buffer[0] = '\0';
    if (ghostty_init_utf8(argc, argv) != 0) {
        startup_error(error_buffer, error_capacity, "initialize libghostty", ghostty_last_init_error());
        return NULL;
    }

    ghostty_clear_error();
    ghostty_config_t config = ghostty_config_new();
    if (!config) {
        startup_error(error_buffer, error_capacity, "allocate the terminal configuration", NULL);
        return NULL;
    }
    ghostty_config_load_default_files(config);
    ghostty_config_load_cli_args(config);
    ghostty_config_load_recursive_files(config);
    // Retain inputs before theme finalization adds theme values to replay.
    ghostty_config_t base_config = ghostty_config_clone(config);
    if (!base_config) {
        startup_error(error_buffer, error_capacity, "clone the terminal configuration", NULL);
        ghostty_config_free(config); return NULL;
    }
    ghostty_config_finalize(config);
    report_theme_colors(config, userdata);
    for (uint32_t i = 0; i < ghostty_config_diagnostics_count(config); ++i) {
        ghostty_diagnostic_s diagnostic = ghostty_config_get_diagnostic(config, i);
        ghostty_egui_config_issue(userdata, diagnostic.message);
    }

    ghostty_runtime_config_s runtime = {
        // App wakeups use the process event loop, not a particular surface.
        .userdata = NULL,
        .wakeup_cb = wake,
        .action_cb = action,
        .read_clipboard_cb = read_clipboard,
        .confirm_read_clipboard_cb = confirm_clipboard,
        .write_clipboard_cb = write_clipboard,
        .close_surface_cb = close_surface,
    };
    ghostty_app_t app = ghostty_app_new(&runtime, config);
    if (!app) {
        startup_error(error_buffer, error_capacity, "create the embedded terminal application", NULL);
        ghostty_config_free(base_config); ghostty_config_free(config); return NULL;
    }
    ghostty_config_free(config);

    ghostty_surface_config_s options = ghostty_surface_config_new();
    options.platform_tag = GHOSTTY_PLATFORM_OPENGL;
    options.platform.opengl.window = window;
    options.platform.opengl.display = display;
    options.platform.opengl.context = context;
    options.scale_factor = scale;
    options.userdata = userdata;
    ghostty_env_var_s token_env[] = {
        { .key = "GHOSTTY_PICKER_ENDPOINT", .value = picker_endpoint },
        { .key = "GHOSTTY_PICKER_TOKEN", .value = picker_token },
    };
    options.env_vars = token_env;
    options.env_var_count = 2;
    ghostty_clear_error();
    ghostty_surface_t surface = ghostty_surface_new(app, &options);
    if (!surface) {
        startup_error(error_buffer, error_capacity, "create the main terminal surface", NULL);
        ghostty_config_free(base_config);
        ghostty_app_free(app);
        return NULL;
    }

    GhosttyEguiHost *host = calloc(1, sizeof(*host));
    if (!host) {
        startup_error(error_buffer, error_capacity, "allocate the main terminal host", strerror(errno));
        ghostty_config_free(base_config);
        ghostty_surface_free(surface);
        ghostty_app_free(app);
        return NULL;
    }
    host->app = app;
    host->surface = surface;
    host->shared = calloc(1, sizeof(*host->shared));
    if (!host->shared) {
        startup_error(error_buffer, error_capacity, "allocate shared terminal application", strerror(errno));
        ghostty_surface_free(surface);
        ghostty_app_free(app);
        ghostty_config_free(base_config);
        free(host);
        return NULL;
    }
    host->shared->base_config = base_config;
    host->shared->hosts = host;
    host->platform = options.platform.opengl;
#ifdef _WIN32
    if (SetPropW((HWND)window, host_property, host)) {
        host->previous_window_proc = (WNDPROC)SetWindowLongPtrW((HWND)window, GWLP_WNDPROC, (LONG_PTR)host_window_proc);
        if (host->previous_window_proc) host->window = (HWND)window;
        else RemovePropW((HWND)window, host_property);
    }
#endif
    return host;
}

GhosttyEguiHost *ghostty_egui_host_create_child(
    GhosttyEguiHost *parent, void *userdata, void *window, void *display, void *context,
    double scale,
    const char *picker_endpoint, const char *picker_token,
    const char *command, const char *cwd,
    char *error_buffer, size_t error_capacity
) {
    if (error_buffer && error_capacity) error_buffer[0] = '\0';
    if (!parent) {
        startup_error(error_buffer, error_capacity, "create an additional terminal", "No parent terminal is available");
        return NULL;
    }
    GhosttyEguiHost *host = calloc(1, sizeof(*host));
    if (!host) {
        startup_error(error_buffer, error_capacity, "allocate an additional terminal host", strerror(errno));
        return NULL;
    }
    ghostty_surface_config_s options = ghostty_surface_config_new();
    options.platform_tag = GHOSTTY_PLATFORM_OPENGL;
    options.platform.opengl = parent->platform;
    if (window) {
        options.platform.opengl.window = window;
        options.platform.opengl.display = display;
        options.platform.opengl.context = context;
    }
    options.scale_factor = scale;
    options.userdata = userdata;
    options.command = command;
    options.working_directory = cwd;
    ghostty_env_var_s token_env[] = {
        { .key = "GHOSTTY_PICKER_ENDPOINT", .value = picker_endpoint },
        { .key = "GHOSTTY_PICKER_TOKEN", .value = picker_token },
    };
    options.env_vars = token_env;
    options.env_var_count = 2;
    host->app = parent->app;
    host->platform = options.platform.opengl;
    host->surface = ghostty_surface_new(host->app, &options);
    if (!host->surface) {
        startup_error(error_buffer, error_capacity, "create an additional terminal surface", NULL);
        free(host);
        return NULL;
    }
    host->shared = parent->shared;
    host->next = host->shared->hosts;
    host->shared->hosts = host;
#ifdef _WIN32
    if (window && SetPropW((HWND)window, host_property, host)) {
        host->previous_window_proc = (WNDPROC)SetWindowLongPtrW((HWND)window, GWLP_WNDPROC, (LONG_PTR)host_window_proc);
        if (host->previous_window_proc) host->window = (HWND)window;
        else RemovePropW((HWND)window, host_property);
    }
#endif
    return host;
}

bool ghostty_egui_host_apply_settings(GhosttyEguiHost *host, const char *name, const char *family, float size, char *error, size_t capacity) {
    if (!host || !host->shared->base_config) return false;
    ghostty_clear_error();
    ghostty_config_t candidate = ghostty_config_clone(host->shared->base_config);
    if (!candidate) {
        snprintf(error, capacity, "Unable to allocate theme configuration");
        return false;
    }
    bool loaded = (!name || ghostty_config_set_theme(candidate, name)) && ghostty_config_set_font(candidate, family, size);
    // Keep the unfinalized inputs for later theme/font updates.
    ghostty_config_t inputs = loaded ? ghostty_config_clone(candidate) : NULL;
    if (loaded && !inputs) loaded = false;
    if (loaded) ghostty_config_finalize(candidate);
    uint32_t count = ghostty_config_diagnostics_count(candidate);
    if (!loaded || count) {
        snprintf(error, capacity, "%s", count ? ghostty_config_get_diagnostic(candidate, 0).message : "Unable to load theme configuration");
        ghostty_config_free(candidate);
        if (inputs) ghostty_config_free(inputs);
        return false;
    }
    // One app owns all window and floating surfaces. The update propagates
    // to existing surfaces and becomes the configuration for future surfaces.
    ghostty_app_update_config(host->app, candidate);
    for (GhosttyEguiHost *item = host->shared->hosts; item; item = item->next)
        report_theme_colors(candidate, ghostty_surface_userdata(item->surface));
    ghostty_config_free(host->shared->base_config);
    host->shared->base_config = inputs;
    ghostty_config_free(candidate);
    return true;
}

bool ghostty_egui_host_apply_theme(GhosttyEguiHost *host, const char *name, char *error, size_t capacity) {
    return ghostty_egui_host_apply_settings(host, name, NULL, 0, error, capacity);
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
    GhosttyEguiHost **link = &host->shared->hosts;
    while (*link && *link != host) link = &(*link)->next;
    if (*link) *link = host->next;
    if (!host->shared->hosts) {
        ghostty_app_free(host->app);
        ghostty_config_free(host->shared->base_config);
        free(host->shared);
    } else {
        bool focused = false;
        for (GhosttyEguiHost *item = host->shared->hosts; item; item = item->next)
            focused |= item->focused;
        ghostty_app_set_focus(host->app, focused);
    }
    free(host);
}

bool ghostty_egui_host_read_font(GhosttyEguiHost *host, bool bold, ghostty_font_data_s *result) {
    ghostty_clear_error();
    bool succeeded = ghostty_surface_read_font(host->surface, bold, result); runtime_error(host, "ghostty_egui_host_read_font"); return succeeded;
}
ghostty_surface_size_s ghostty_egui_host_surface_size(GhosttyEguiHost *host) {
    return ghostty_surface_size(host->surface);
}
void ghostty_egui_host_tick(GhosttyEguiHost *host) {
    ghostty_clear_error(); ghostty_app_tick(host->app);  runtime_error(host, "ghostty_egui_host_tick"); }
void ghostty_egui_host_resize(GhosttyEguiHost *host, uint32_t width, uint32_t height, double scale) {
    ghostty_clear_error();
    ghostty_surface_set_content_scale(host->surface, scale, scale);
    runtime_error(host, "set terminal content scale");
    ghostty_surface_set_size(host->surface, width, height);
 runtime_error(host, "ghostty_egui_host_resize"); }
void ghostty_egui_host_focus(GhosttyEguiHost *host, bool focused) {
    ghostty_clear_error();
    host->focused = focused;
    bool app_focused = false;
    for (GhosttyEguiHost *item = host->shared->hosts; item; item = item->next)
        app_focused |= item->focused;
    ghostty_app_set_focus(host->app, app_focused);
    runtime_error(host, "set terminal app focus");
    ghostty_surface_set_focus(host->surface, focused);
 runtime_error(host, "ghostty_egui_host_focus"); }
bool ghostty_egui_host_take_frame(GhosttyEguiHost *host, ghostty_gl_frame_s *frame) {
    bool succeeded = ghostty_surface_take_gl_frame(host->surface, frame);
    runtime_error(host, "acquire terminal frame");
    return succeeded;
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
    ghostty_clear_error();
    bool succeeded = ghostty_surface_key_usb(host->surface, action_value, mods, usb, unshifted, text); runtime_error(host, "ghostty_egui_host_key"); return succeeded;
}
bool ghostty_egui_host_snapshot_begin(GhosttyEguiHost *host, uint64_t *snapshot_generation) {
    ghostty_clear_error();
    bool succeeded = ghostty_surface_snapshot_begin(host->surface, snapshot_generation); runtime_error(host, "ghostty_egui_host_snapshot_begin"); return succeeded;
}
bool ghostty_egui_host_selection_text(GhosttyEguiHost *host, ghostty_snapshot_selection_text_s *result) {
    ghostty_clear_error();
    ghostty_text_s text = {0};
    memset(result, 0, sizeof(*result));
    if (!ghostty_surface_read_selection(host->surface, &text)) {
        runtime_error(host, "read terminal selection");
        return false;
    }
    char *copy = malloc(text.text_len + 1);
    if (!copy) ghostty_egui_runtime_issue(ghostty_surface_userdata(host->surface), "copy terminal selection", strerror(errno));
    if (copy) {
        if (text.text_len) memcpy(copy, text.text, text.text_len);
        copy[text.text_len] = 0;
        result->text = copy;
        result->len = text.text_len;
    }
    ghostty_surface_free_text(host->surface, &text);
    return copy != NULL;
}
void ghostty_egui_host_selection_text_free(ghostty_snapshot_selection_text_s *result) {
    free((void *)result->text);
    memset(result, 0, sizeof(*result));
}
bool ghostty_egui_host_snapshot_end(GhosttyEguiHost *host, uint64_t snapshot_generation) {
    return ghostty_surface_snapshot_end(host->surface, snapshot_generation);
}
bool ghostty_egui_host_snapshot_view(
    GhosttyEguiHost *host, uint64_t generation, ghostty_snapshot_view_s *view
) {
    ghostty_clear_error(); bool succeeded = ghostty_surface_snapshot_view(host->surface, generation, view); runtime_error(host, "ghostty_egui_host_snapshot_view"); return succeeded; }
bool ghostty_egui_host_snapshot_resolve_cell(
    GhosttyEguiHost *host, uint64_t generation, ghostty_snapshot_cell_s requested,
    bool snap_right, ghostty_snapshot_cell_s *actual
) {
    ghostty_clear_error(); bool succeeded = ghostty_surface_snapshot_resolve_cell(
    host->surface, generation, requested, snap_right, actual
); runtime_error(host, "ghostty_egui_host_snapshot_resolve_cell"); return succeeded; }
bool ghostty_egui_host_snapshot_resize(GhosttyEguiHost *host, uint64_t generation,
    ghostty_snapshot_reflow_map_s *result) {
    ghostty_clear_error();
    return ghostty_surface_snapshot_resize(host->surface, generation, result);
}
bool ghostty_egui_host_snapshot_selection_text(
    GhosttyEguiHost *host, uint64_t generation,
    ghostty_snapshot_cell_range_s range, bool trim, bool rectangle,
    ghostty_snapshot_selection_text_s *result
) {
    ghostty_clear_error(); bool succeeded = ghostty_surface_snapshot_selection_text(host->surface, generation, range, trim, rectangle, result); runtime_error(host, "ghostty_egui_host_snapshot_selection_text"); return succeeded; }
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
) {
    ghostty_clear_error(); bool succeeded = ghostty_surface_snapshot_text_row(host->surface, generation, row, result); runtime_error(host, "ghostty_egui_host_snapshot_text_row"); return succeeded; }
void ghostty_egui_host_snapshot_text_row_free(ghostty_snapshot_text_row_s *result) {
    ghostty_surface_snapshot_text_row_free(result);
}
bool ghostty_egui_host_snapshot_style_runs(GhosttyEguiHost *host, uint64_t generation, uint64_t row, ghostty_snapshot_style_runs_s *result) {
    return ghostty_surface_snapshot_style_runs(host->surface, generation, row, result);
}

bool ghostty_egui_host_snapshot_logical_position(GhosttyEguiHost *host, uint64_t generation, ghostty_snapshot_cell_s cell, ghostty_snapshot_logical_position_s *result) {
    return ghostty_surface_snapshot_logical_position(host->surface, generation, cell, result);
}
bool ghostty_egui_host_snapshot_resolve_logical_range(GhosttyEguiHost *host, uint64_t generation, ghostty_snapshot_logical_range_s range, ghostty_snapshot_resolved_range_s *result) {
    return ghostty_surface_snapshot_resolve_logical_range(host->surface, generation, range, result);
}
bool ghostty_egui_host_live_cursor_line(GhosttyEguiHost *host, ghostty_live_cursor_line_s *result) {
    ghostty_clear_error();
    bool succeeded = ghostty_surface_live_cursor_line(host->surface, result); runtime_error(host, "ghostty_egui_host_live_cursor_line"); return succeeded;
}
bool ghostty_egui_host_live_cursor_position(GhosttyEguiHost *host, ghostty_live_cursor_position_s *result) {
    ghostty_clear_error();
    bool succeeded = ghostty_surface_live_cursor_position(host->surface, result); runtime_error(host, "ghostty_egui_host_live_cursor_position"); return succeeded;
}
void ghostty_egui_host_live_cursor_line_free(ghostty_live_cursor_line_s *result) {
    ghostty_surface_live_cursor_line_free(result);
}
bool ghostty_egui_host_snapshot_apply_decorations(
    GhosttyEguiHost *host, uint64_t generation,
    const ghostty_snapshot_decoration_layer_s *layers, size_t count,
    const ghostty_snapshot_viewport_options_s *viewport
) {
    ghostty_clear_error(); bool succeeded = ghostty_surface_snapshot_apply_decorations(
    host->surface, generation, layers, count, NULL, 0, viewport
); runtime_error(host, "ghostty_egui_host_snapshot_apply_decorations"); return succeeded; }
bool ghostty_egui_host_snapshot_apply_cell_text(
    GhosttyEguiHost *host, uint64_t generation,
    const ghostty_snapshot_decoration_layer_s *layers, size_t count,
    const ghostty_snapshot_cell_text_s *text, size_t text_count,
    const ghostty_snapshot_viewport_options_s *viewport
) {
    ghostty_clear_error(); bool succeeded = ghostty_surface_snapshot_apply_decorations(
    host->surface, generation, layers, count, text, text_count, viewport
); runtime_error(host, "ghostty_egui_host_snapshot_apply_cell_text"); return succeeded; }
bool ghostty_egui_host_snapshot_decorations_need_refresh(
    GhosttyEguiHost *host, uint64_t generation
) {
    ghostty_clear_error(); bool succeeded = ghostty_surface_snapshot_decorations_need_refresh(host->surface, generation); runtime_error(host, "ghostty_egui_host_snapshot_decorations_need_refresh"); return succeeded; }
void ghostty_egui_host_text(GhosttyEguiHost *host, const char *text, size_t len) {
    ghostty_clear_error();
    ghostty_surface_text(host->surface, text, len);
 runtime_error(host, "ghostty_egui_host_text"); }
void ghostty_egui_host_preedit(GhosttyEguiHost *host, const char *text, size_t len) {
    ghostty_clear_error();
    ghostty_surface_preedit(host->surface, text, len);
 runtime_error(host, "ghostty_egui_host_preedit"); }
void ghostty_egui_host_mouse_pos(GhosttyEguiHost *host, double x, double y, int mods) {
    ghostty_clear_error();
    ghostty_surface_mouse_pos(host->surface, x, y, (ghostty_input_mods_e)mods);
 runtime_error(host, "ghostty_egui_host_mouse_pos"); }
void ghostty_egui_host_mouse_button(GhosttyEguiHost *host, int state, int button, int mods) {
    ghostty_clear_error();
    ghostty_surface_mouse_button(host->surface,
        (ghostty_input_mouse_state_e)state,
        (ghostty_input_mouse_button_e)button,
        (ghostty_input_mods_e)mods);
 runtime_error(host, "ghostty_egui_host_mouse_button"); }
void ghostty_egui_host_mouse_scroll(GhosttyEguiHost *host, double x, double y, int mods) {
    ghostty_clear_error();
    ghostty_surface_mouse_scroll(host->surface, x, y, mods);
 runtime_error(host, "ghostty_egui_host_mouse_scroll"); }

uint64_t ghostty_egui_host_shell_pid(GhosttyEguiHost *host) { return ghostty_surface_shell_pid(host->surface); }
uint64_t ghostty_egui_host_shell_creation_time(GhosttyEguiHost *host) { return ghostty_surface_shell_creation_time(host->surface); }
