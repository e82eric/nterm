#include <ghostty/vt/terminal.h>
#include <ghostty/vt/grid_ref.h>
#include <ghostty/vt/style.h>
#include <ghostty/vt/color.h>
#include <string.h>

typedef struct {
    uint32_t codepoints[8];
    uint32_t foreground;
    uint32_t background;
    uint32_t underline_color;
    uint8_t length;
    uint8_t flags;
    uint8_t underline;
    uint8_t reserved;
} GhosttyEguiPreviewCell;

static uint32_t rgb(GhosttyColorRgb color) {
    return ((uint32_t)color.r << 16) | ((uint32_t)color.g << 8) | color.b;
}

static uint32_t style_color(GhosttyStyleColor color,
                            const GhosttyColorRgb *palette,
                            uint32_t fallback) {
    if (color.tag == GHOSTTY_STYLE_COLOR_RGB) return rgb(color.value.rgb);
    if (color.tag == GHOSTTY_STYLE_COLOR_PALETTE)
        return rgb(palette[color.value.palette]);
    return fallback;
}

// An isolated VT terminal: no PTY, shell, or GPU renderer is created.
bool ghostty_egui_preview_parse(const uint8_t *bytes, size_t length,
                                uint16_t columns, uint16_t rows,
                                size_t scroll_offset,
                                GhosttyEguiPreviewCell *cells,
                                size_t *out_total_rows) {
    if (!cells || !out_total_rows || !columns || !rows || (length && !bytes)) return false;
    memset(cells, 0, (size_t)columns * rows * sizeof(*cells));
    GhosttyTerminal terminal = NULL;
    if (ghostty_terminal_new(NULL, &terminal, columns, rows) != GHOSTTY_SUCCESS)
        return false;
    size_t max_lines = 8192;
    if (ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES,
                             &max_lines) != GHOSTTY_SUCCESS) {
        ghostty_terminal_free(terminal);
        return false;
    }
    ghostty_terminal_vt_write(terminal, bytes, length);
    size_t total_rows = 0;
    if (ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_TOTAL_ROWS,
                             &total_rows) != GHOSTTY_SUCCESS) {
        ghostty_terminal_free(terminal);
        return false;
    }
    *out_total_rows = total_rows;
    size_t max_offset = total_rows > rows ? total_rows - rows : 0;
    size_t first_row = scroll_offset < max_offset ? scroll_offset : max_offset;
    GhosttyColorRgb palette[256];
    ghostty_color_palette_default(palette);
    for (uint32_t y = 0; y < rows; ++y) {
        for (uint16_t x = 0; x < columns; ++x) {
            GhosttyPoint point = {0};
            point.tag = GHOSTTY_POINT_TAG_SCREEN;
            point.value.coordinate.x = x;
            point.value.coordinate.y = first_row + y;
            GhosttyGridRef ref = {0};
            ref.size = sizeof(ref);
            if (ghostty_terminal_grid_ref(terminal, point, &ref) != GHOSTTY_SUCCESS)
                continue;
            GhosttyEguiPreviewCell *out = &cells[(size_t)y * columns + x];
            size_t count = 0;
            if (ghostty_grid_ref_graphemes(&ref, out->codepoints, 8, &count) == GHOSTTY_SUCCESS)
                out->length = (uint8_t)count;
            GhosttyStyle style = {0};
            style.size = sizeof(style);
            if (ghostty_grid_ref_style(&ref, &style) != GHOSTTY_SUCCESS)
                continue;
            out->foreground = style_color(style.fg_color, palette, 0xebdbb2);
            out->background = style_color(style.bg_color, palette, 0x282828);
            out->underline_color = style_color(style.underline_color, palette, out->foreground);
            out->flags = (style.bold ? 1 : 0) | (style.italic ? 2 : 0) |
                         (style.strikethrough ? 4 : 0) | (style.inverse ? 8 : 0) |
                         (style.invisible ? 16 : 0) | (style.faint ? 32 : 0);
            out->underline = style.underline ? 1 : 0;
        }
    }
    ghostty_terminal_free(terminal);
    return true;
}
