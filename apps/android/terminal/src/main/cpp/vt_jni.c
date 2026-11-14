// VtJni 的 native 方法。一份 libghostty-vt 状态机连同读屏幕、编码输入要用的对象装在一个 Vt 里，句柄是它的指针
// 转成的 jlong；由 Kotlin 那边保证同一时刻只有一个线程用一个句柄、关掉后不再传进来。

#include <android/data_space.h>
#include <android/imagedecoder.h>
#include <jni.h>
#include <limits.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include <ghostty/vt.h>

#include "snapshot_layout.h"

typedef struct {
    GhosttyTerminal terminal;
    GhosttyRenderState render_state;
    GhosttyRenderStateRowIterator rows;
    GhosttyRenderStateRowCells cells;
    GhosttyRenderStateImageIterator images;
    GhosttyKeyEncoder key_encoder;
    GhosttyKeyEvent key_event;
    GhosttyMouseEncoder mouse_encoder;
    GhosttyMouseEvent mouse_event;
    /** 程序用同步输出（mode 2026）冻结屏幕的时刻，CLOCK_MONOTONIC 的纳秒；没冻结时为 -1。 */
    int64_t held_since_ns;
    /** 上次交出图片列表时的 GHOSTTY_RENDER_STATE_DATA_IMAGE_PLACEMENTS_GENERATION。 */
    bool has_image_list;
    uint64_t image_list;
    /** 这一帧推动动画后下一帧离现在多少毫秒，没有在播的为 -1；缓冲区不够、重来的那次沿用。 */
    int64_t next_image_frame_ms;
    /** 一帧里多码点的字素簇，写完行再整块抄到缓冲区末尾。 */
    uint32_t *graphemes;
    size_t graphemes_len;
    size_t graphemes_cap;
} Vt;

static Vt *vt_of(jlong handle) { return (Vt *)(intptr_t)handle; }

static int64_t monotonic_ns(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (int64_t)now.tv_sec * 1000000000 + now.tv_nsec;
}

static int32_t rgb_of(GhosttyColorRgb color) { return (int32_t)color.r << 16 | (int32_t)color.g << 8 | color.b; }

static GhosttyColorRgb ghostty_rgb(jint rgb) {
    GhosttyColorRgb color = {.r = (uint8_t)(rgb >> 16), .g = (uint8_t)(rgb >> 8), .b = (uint8_t)rgb};
    return color;
}

static jbyteArray byte_array(JNIEnv *env, const uint8_t *bytes, size_t len) {
    jbyteArray array = (*env)->NewByteArray(env, (jsize)len);
    if (array != NULL && len > 0) {
        (*env)->SetByteArrayRegion(env, array, 0, (jsize)len, (const jbyte *)bytes);
    }
    return array;
}

// MARK: PNG

// Kitty 图片里的 PNG 交给系统的 AImageDecoder 解成不预乘的 8 位 sRGB RGBA，缓冲区用 libghostty 给的分配器分配
// （它限着单张图片的大小）。整个进程共用，库加载时装上。
static bool decode_png(void *userdata, const GhosttyAllocator *allocator, const uint8_t *data, size_t data_len,
                       GhosttySysImage *out) {
    (void)userdata;
    if (data == NULL || out == NULL) {
        return false;
    }
    AImageDecoder *decoder = NULL;
    if (AImageDecoder_createFromBuffer(data, data_len, &decoder) != ANDROID_IMAGE_DECODER_SUCCESS) {
        return false;
    }
    bool ok = AImageDecoder_setAndroidBitmapFormat(decoder, ANDROID_BITMAP_FORMAT_RGBA_8888) ==
                  ANDROID_IMAGE_DECODER_SUCCESS &&
              AImageDecoder_setUnpremultipliedRequired(decoder, true) == ANDROID_IMAGE_DECODER_SUCCESS &&
              AImageDecoder_setDataSpace(decoder, ADATASPACE_SRGB) == ANDROID_IMAGE_DECODER_SUCCESS;
    const AImageDecoderHeaderInfo *info = AImageDecoder_getHeaderInfo(decoder);
    int32_t width = info != NULL ? AImageDecoderHeaderInfo_getWidth(info) : 0;
    int32_t height = info != NULL ? AImageDecoderHeaderInfo_getHeight(info) : 0;
    size_t stride = (size_t)width * 4;
    uint8_t *pixels = NULL;
    size_t length = 0;
    if (ok && width > 0 && height > 0 && stride <= SIZE_MAX / (size_t)height) {
        length = stride * (size_t)height;
        pixels = ghostty_alloc(allocator, length);
    }
    ok = pixels != NULL && AImageDecoder_decodeImage(decoder, pixels, stride, length) == ANDROID_IMAGE_DECODER_SUCCESS;
    AImageDecoder_delete(decoder);
    if (!ok) {
        if (pixels != NULL) {
            ghostty_free(allocator, pixels, length);
        }
        return false;
    }
    out->width = (uint32_t)width;
    out->height = (uint32_t)height;
    out->data = pixels;
    out->data_len = length;
    return true;
}

JNIEXPORT jint JNI_OnLoad(JavaVM *vm, void *reserved) {
    (void)vm;
    (void)reserved;
    ghostty_sys_set(GHOSTTY_SYS_OPT_DECODE_PNG, (const void *)decode_png);
    return JNI_VERSION_1_6;
}

// MARK: 建和释放

static void on_render_hold(GhosttyTerminal terminal, void *userdata, bool held) {
    (void)terminal;
    ((Vt *)userdata)->held_since_ns = held ? monotonic_ns() : -1;
}

static void vt_free(Vt *vt) {
    if (vt->mouse_event != NULL) ghostty_mouse_event_free(vt->mouse_event);
    if (vt->mouse_encoder != NULL) ghostty_mouse_encoder_free(vt->mouse_encoder);
    if (vt->key_event != NULL) ghostty_key_event_free(vt->key_event);
    if (vt->key_encoder != NULL) ghostty_key_encoder_free(vt->key_encoder);
    if (vt->cells != NULL) ghostty_render_state_row_cells_free(vt->cells);
    if (vt->images != NULL) ghostty_render_state_image_iterator_free(vt->images);
    if (vt->rows != NULL) ghostty_render_state_row_iterator_free(vt->rows);
    if (vt->render_state != NULL) ghostty_render_state_free(vt->render_state);
    if (vt->terminal != NULL) ghostty_terminal_free(vt->terminal);
    free(vt->graphemes);
    free(vt);
}

// 建一份 VT，挂上同步输出的回调，渲染时多取视口上面一行（平滑滚动时画面往下错开，顶上要露出它的一部分）。
// 建不出来时返回 0。
JNIEXPORT jlong JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeNew(JNIEnv *env, jclass cls, jint cols,
                                                                        jint rows) {
    (void)env;
    (void)cls;
    Vt *vt = calloc(1, sizeof(Vt));
    if (vt == NULL) {
        return 0;
    }
    vt->held_since_ns = -1;
    vt->next_image_frame_ms = -1;
    if (ghostty_terminal_new(NULL, &vt->terminal, (uint16_t)cols, (uint16_t)rows) != GHOSTTY_SUCCESS ||
        ghostty_render_state_new(NULL, &vt->render_state) != GHOSTTY_SUCCESS ||
        ghostty_render_state_row_iterator_new(NULL, &vt->rows) != GHOSTTY_SUCCESS ||
        ghostty_render_state_row_cells_new(NULL, &vt->cells) != GHOSTTY_SUCCESS ||
        ghostty_render_state_image_iterator_new(NULL, &vt->images) != GHOSTTY_SUCCESS ||
        ghostty_key_encoder_new(NULL, &vt->key_encoder) != GHOSTTY_SUCCESS ||
        ghostty_key_event_new(NULL, &vt->key_event) != GHOSTTY_SUCCESS ||
        ghostty_mouse_encoder_new(NULL, &vt->mouse_encoder) != GHOSTTY_SUCCESS ||
        ghostty_mouse_event_new(NULL, &vt->mouse_event) != GHOSTTY_SUCCESS) {
        vt_free(vt);
        return 0;
    }
    ghostty_terminal_set(vt->terminal, GHOSTTY_TERMINAL_OPT_USERDATA, vt);
    ghostty_terminal_set(vt->terminal, GHOSTTY_TERMINAL_OPT_RENDER_HOLD, (const void *)on_render_hold);
    GhosttyRenderStateOverscan overscan = {.above = 1, .below = 0};
    ghostty_render_state_set(vt->render_state, GHOSTTY_RENDER_STATE_OPT_OVERSCAN, &overscan);
    return (jlong)(intptr_t)vt;
}

JNIEXPORT void JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeFree(JNIEnv *env, jclass cls, jlong handle) {
    (void)env;
    (void)cls;
    vt_free(vt_of(handle));
}

// MARK: 喂字节、改尺寸、换主题

JNIEXPORT void JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeFeed(JNIEnv *env, jclass cls, jlong handle,
                                                                        jbyteArray bytes, jint offset, jint length) {
    (void)cls;
    jbyte *data = (*env)->GetByteArrayElements(env, bytes, NULL);
    if (data == NULL) {
        return;
    }
    ghostty_terminal_vt_write(vt_of(handle)->terminal, (const uint8_t *)data + offset, (size_t)length);
    (*env)->ReleaseByteArrayElements(env, bytes, data, JNI_ABORT);
}

JNIEXPORT void JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeResize(JNIEnv *env, jclass cls, jlong handle,
                                                                          jint cols, jint rows, jint cell_width,
                                                                          jint cell_height) {
    (void)env;
    (void)cls;
    ghostty_terminal_resize(vt_of(handle)->terminal, (uint16_t)cols, (uint16_t)rows, (uint32_t)cell_width,
                            (uint32_t)cell_height);
}

JNIEXPORT void JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeConfigureCommon(
    JNIEnv *env, jclass cls, jlong handle, jlong scrollback_lines, jlong scrollback_bytes, jlong unknown_max_bytes,
    jlong image_storage_bytes) {
    (void)env;
    (void)cls;
    GhosttyTerminal terminal = vt_of(handle)->terminal;
    size_t lines = (size_t)scrollback_lines;
    size_t bytes = (size_t)scrollback_bytes;
    size_t unknown = (size_t)unknown_max_bytes;
    uint64_t images = (uint64_t)image_storage_bytes;
    bool no = false;
    ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES, &lines);
    ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, &bytes);
    ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_UNKNOWN_MAX_BYTES, &unknown);
    ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT, &images);
    ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_FILE, &no);
    ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_TEMP_FILE, NULL);
    ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_SHARED_MEM, &no);
}

static bool mode_value(GhosttyTerminal terminal, uint16_t value, bool ansi) {
    GhosttyTerminalModeConfig config = {.mode = ghostty_mode_new(value, ansi), .value = false};
    if (ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_MODE, &config) != GHOSTTY_SUCCESS) {
        return false;
    }
    return config.value;
}

// 宿主的 `vt::apply_theme` 的步骤，配置项之外的部分（`configure_common`）由调用方接着做。palette 每项是
// 下标 << 24 | 0xRRGGBB，cursor 为 -1 时光标色跟随单元格。读不出内置调色板时什么都不改，返回假。
JNIEXPORT jboolean JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeApplyTheme(
    JNIEnv *env, jclass cls, jlong handle, jint background, jint foreground, jint cursor, jintArray palette_entries,
    jint cursor_style, jboolean cursor_blink) {
    (void)cls;
    GhosttyTerminal terminal = vt_of(handle)->terminal;
    ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_COLOR_PALETTE, NULL);
    GhosttyColorRgb palette[256];
    if (ghostty_terminal_get(terminal, GHOSTTY_TERMINAL_DATA_COLOR_PALETTE_DEFAULT, palette) != GHOSTTY_SUCCESS) {
        return JNI_FALSE;
    }
    jsize count = (*env)->GetArrayLength(env, palette_entries);
    jint *entries = (*env)->GetIntArrayElements(env, palette_entries, NULL);
    if (entries == NULL) {
        return JNI_FALSE;
    }
    for (jsize i = 0; i < count; i++) {
        palette[(uint32_t)entries[i] >> 24] = ghostty_rgb(entries[i]);
    }
    (*env)->ReleaseIntArrayElements(env, palette_entries, entries, JNI_ABORT);
    GhosttyColorRgb background_rgb = ghostty_rgb(background);
    GhosttyColorRgb foreground_rgb = ghostty_rgb(foreground);
    ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND, &background_rgb);
    ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND, &foreground_rgb);
    // 跟随单元格的光标色画的时候按光标所在格子解析，VT 里不设默认值。
    if (cursor >= 0) {
        GhosttyColorRgb cursor_rgb = ghostty_rgb(cursor);
        ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_COLOR_CURSOR, &cursor_rgb);
    } else {
        ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_COLOR_CURSOR, NULL);
    }
    // 宿主的 `vt::set_default_cursor`：程序没用 DECSCUSR 设过形状时，libghostty 改默认值会把当前的闪烁一起换掉；
    // 先换形状，看闪烁是不是变回了旧默认值，不一样说明程序用模式 12 改过，换完默认闪烁后还原它。
    bool blinking = mode_value(terminal, 12, false);
    GhosttyCursorStyle style = (GhosttyCursorStyle)cursor_style;
    ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_DEFAULT_CURSOR_STYLE, &style);
    bool old_default = mode_value(terminal, 12, false);
    bool blink = cursor_blink;
    ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_DEFAULT_CURSOR_BLINK, &blink);
    if (blinking != old_default) {
        GhosttyTerminalModeConfig config = {.mode = ghostty_mode_new(12, false), .value = blinking};
        ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_MODE, &config);
    }
    ghostty_terminal_set(terminal, GHOSTTY_TERMINAL_OPT_COLOR_PALETTE, palette);
    return JNI_TRUE;
}

// MARK: 模式和状态

JNIEXPORT jlong JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeRenderHeldSince(JNIEnv *env, jclass cls,
                                                                                    jlong handle) {
    (void)env;
    (void)cls;
    return vt_of(handle)->held_since_ns;
}

JNIEXPORT jboolean JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeMode(JNIEnv *env, jclass cls, jlong handle,
                                                                            jint value, jboolean ansi) {
    (void)env;
    (void)cls;
    return mode_value(vt_of(handle)->terminal, (uint16_t)value, ansi);
}

JNIEXPORT jboolean JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeMouseTracking(JNIEnv *env, jclass cls,
                                                                                     jlong handle) {
    (void)env;
    (void)cls;
    bool tracking = false;
    if (ghostty_terminal_get(vt_of(handle)->terminal, GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING, &tracking) !=
        GHOSTTY_SUCCESS) {
        return JNI_FALSE;
    }
    return tracking;
}

JNIEXPORT jboolean JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeAlternateScreen(JNIEnv *env, jclass cls,
                                                                                       jlong handle) {
    (void)env;
    (void)cls;
    GhosttyTerminalScreen screen = GHOSTTY_TERMINAL_SCREEN_PRIMARY;
    ghostty_terminal_get(vt_of(handle)->terminal, GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN, &screen);
    return screen == GHOSTTY_TERMINAL_SCREEN_ALTERNATE;
}

JNIEXPORT jlong JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeScrollbackMaxBytes(JNIEnv *env, jclass cls,
                                                                                       jlong handle) {
    (void)env;
    (void)cls;
    size_t bytes = 0;
    ghostty_terminal_get(vt_of(handle)->terminal, GHOSTTY_TERMINAL_DATA_SCROLLBACK_MAX_BYTES, &bytes);
    return (jlong)bytes;
}

// MARK: 视口滚动

// 回滚历史的总行数、视口顶在第几行、视口多高写进 out；读不出时返回假。
JNIEXPORT jboolean JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeScrollbar(JNIEnv *env, jclass cls,
                                                                                 jlong handle, jlongArray out) {
    (void)cls;
    GhosttyTerminalScrollbar bar = {0};
    if (ghostty_terminal_get(vt_of(handle)->terminal, GHOSTTY_TERMINAL_DATA_SCROLLBAR, &bar) != GHOSTTY_SUCCESS) {
        return JNI_FALSE;
    }
    jlong values[3] = {(jlong)bar.total, (jlong)bar.offset, (jlong)bar.len};
    (*env)->SetLongArrayRegion(env, out, 0, 3, values);
    return JNI_TRUE;
}

JNIEXPORT void JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeScrollToBottom(JNIEnv *env, jclass cls,
                                                                                  jlong handle) {
    (void)env;
    (void)cls;
    GhosttyTerminalScrollViewport behavior = {0};
    behavior.tag = GHOSTTY_TERMINAL_SCROLL_VIEWPORT_BOTTOM;
    ghostty_terminal_scroll_viewport(vt_of(handle)->terminal, behavior);
}

// 视口挪 delta 行，负数往上看更早的内容。
JNIEXPORT void JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeScrollBy(JNIEnv *env, jclass cls, jlong handle,
                                                                            jlong delta) {
    (void)env;
    (void)cls;
    GhosttyTerminalScrollViewport behavior = {0};
    behavior.tag = GHOSTTY_TERMINAL_SCROLL_VIEWPORT_DELTA;
    behavior.value.delta = (intptr_t)delta;
    ghostty_terminal_scroll_viewport(vt_of(handle)->terminal, behavior);
}

// MARK: 读屏幕

/** 往缓冲区写一帧。写到 cap 之外的部分丢掉、只记长度，写完后长度超过 cap 时调用方按长度扩容重来。 */
typedef struct {
    uint8_t *base;
    size_t cap;
    size_t len;
} Out;

static void put8(Out *out, size_t at, uint8_t value) {
    if (at + 1 <= out->cap) out->base[at] = value;
}

static void put32(Out *out, size_t at, int32_t value) {
    if (at + 4 <= out->cap) memcpy(out->base + at, &value, 4);
}

static void put64(Out *out, size_t at, int64_t value) {
    if (at + 8 <= out->cap) memcpy(out->base + at, &value, 8);
}

static void put_zero(Out *out, size_t at, size_t len) {
    if (at + len <= out->cap) memset(out->base + at, 0, len);
}

static bool push_grapheme(Vt *vt, uint32_t value) {
    if (vt->graphemes_len == vt->graphemes_cap) {
        size_t cap = vt->graphemes_cap ? vt->graphemes_cap * 2 : 64;
        uint32_t *grown = realloc(vt->graphemes, cap * sizeof(uint32_t));
        if (grown == NULL) {
            return false;
        }
        vt->graphemes = grown;
        vt->graphemes_cap = cap;
    }
    vt->graphemes[vt->graphemes_len++] = value;
    return true;
}

// 当前格子的字素簇按 SNAPSHOT_CELL_TEXT 的写法记到 *text；内存不够时返回假。
static bool read_grapheme(Vt *vt, int32_t *text) {
    uint32_t stack[16];
    GhosttyCodepointBuffer buffer = {.ptr = stack, .cap = 16, .len = 0};
    GhosttyResult result =
        ghostty_render_state_row_cells_get(vt->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF, &buffer);
    uint32_t *heap = NULL;
    if (result == GHOSTTY_OUT_OF_SPACE && buffer.len > 16) {
        heap = malloc(buffer.len * sizeof(uint32_t));
        if (heap == NULL) {
            return false;
        }
        buffer = (GhosttyCodepointBuffer){.ptr = heap, .cap = buffer.len, .len = 0};
        result =
            ghostty_render_state_row_cells_get(vt->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF, &buffer);
    }
    bool ok = true;
    *text = 0;
    if (result == GHOSTTY_SUCCESS && buffer.len == 1) {
        *text = (int32_t)buffer.ptr[0];
    } else if (result == GHOSTTY_SUCCESS && buffer.len > 1) {
        *text = SNAPSHOT_CELL_TEXT_GRAPHEME | (int32_t)vt->graphemes_len;
        ok = push_grapheme(vt, (uint32_t)buffer.len);
        for (size_t i = 0; ok && i < buffer.len; i++) {
            ok = push_grapheme(vt, buffer.ptr[i]);
        }
    }
    free(heap);
    return ok;
}

static uint8_t underline_of(int underline) {
    switch (underline) {
    case GHOSTTY_SGR_UNDERLINE_NONE: return SNAPSHOT_UNDERLINE_NONE;
    case GHOSTTY_SGR_UNDERLINE_DOUBLE: return SNAPSHOT_UNDERLINE_DOUBLE;
    case GHOSTTY_SGR_UNDERLINE_CURLY: return SNAPSHOT_UNDERLINE_CURLY;
    case GHOSTTY_SGR_UNDERLINE_DOTTED: return SNAPSHOT_UNDERLINE_DOTTED;
    case GHOSTTY_SGR_UNDERLINE_DASHED: return SNAPSHOT_UNDERLINE_DASHED;
    default: return SNAPSHOT_UNDERLINE_SINGLE;
    }
}

static const GhosttyCellData cell_keys[] = {
    GHOSTTY_CELL_DATA_CONTENT_TAG, GHOSTTY_CELL_DATA_WIDE, GHOSTTY_CELL_DATA_HAS_TEXT, GHOSTTY_CELL_DATA_HAS_STYLING,
};

// 一屏大多是空白格和不带样式的字，所以先从原始格子一次取齐有没有字、样式、单设的背景色，没有的就不再一项项去问
// 颜色和样式。内存不够时返回假。
static bool write_cell(Vt *vt, Out *out, size_t at) {
    GhosttyCell raw = 0;
    GhosttyCellContentTag tag = GHOSTTY_CELL_CONTENT_CODEPOINT;
    GhosttyCellWide wide = GHOSTTY_CELL_WIDE_NARROW;
    bool has_text = false;
    bool styled = false;
    void *values[] = {&tag, &wide, &has_text, &styled};
    put_zero(out, at, SNAPSHOT_CELL_SIZE);
    if (ghostty_render_state_row_cells_get(vt->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW, &raw) !=
            GHOSTTY_SUCCESS ||
        ghostty_cell_get_multi(raw, sizeof(cell_keys) / sizeof(cell_keys[0]), cell_keys, values, NULL) !=
            GHOSTTY_SUCCESS) {
        return true;
    }
    uint8_t width = SNAPSHOT_WIDTH_NARROW;
    switch (wide) {
    case GHOSTTY_CELL_WIDE_WIDE: width = SNAPSHOT_WIDTH_WIDE; break;
    case GHOSTTY_CELL_WIDE_SPACER_TAIL: width = SNAPSHOT_WIDTH_SPACER_TAIL; break;
    case GHOSTTY_CELL_WIDE_SPACER_HEAD: width = SNAPSHOT_WIDTH_SPACER_HEAD; break;
    default: break;
    }
    put8(out, at + SNAPSHOT_CELL_WIDTH, width);
    if (has_text) {
        int32_t text = 0;
        if (!read_grapheme(vt, &text)) {
            return false;
        }
        put32(out, at + SNAPSHOT_CELL_TEXT, text);
    }
    // 前景色只来自样式，背景色来自样式或只有背景色的格子（`CONTENT_TAG` 是 `BG_COLOR_*`）。
    bool only_text = tag == GHOSTTY_CELL_CONTENT_CODEPOINT || tag == GHOSTTY_CELL_CONTENT_CODEPOINT_GRAPHEME;
    if (!styled && only_text) {
        return true;
    }
    GhosttyColorRgb color;
    if (ghostty_render_state_row_cells_get(vt->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR, &color) ==
        GHOSTTY_SUCCESS) {
        put32(out, at + SNAPSHOT_CELL_FOREGROUND, SNAPSHOT_COLOR_SET | rgb_of(color));
    }
    if (ghostty_render_state_row_cells_get(vt->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR, &color) ==
        GHOSTTY_SUCCESS) {
        put32(out, at + SNAPSHOT_CELL_BACKGROUND, SNAPSHOT_COLOR_SET | rgb_of(color));
    }
    GhosttyStyle style;
    memset(&style, 0, sizeof(style));
    if (styled && ghostty_render_state_row_cells_get(vt->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE, &style) ==
                      GHOSTTY_SUCCESS) {
        GhosttyStyleFlags flags = style.flags;
        uint8_t attributes = 0;
        if (flags & GHOSTTY_STYLE_FLAG_BOLD) attributes |= SNAPSHOT_ATTRIBUTE_BOLD;
        if (flags & GHOSTTY_STYLE_FLAG_ITALIC) attributes |= SNAPSHOT_ATTRIBUTE_ITALIC;
        if (flags & GHOSTTY_STYLE_FLAG_FAINT) attributes |= SNAPSHOT_ATTRIBUTE_FAINT;
        if (flags & GHOSTTY_STYLE_FLAG_INVERSE) attributes |= SNAPSHOT_ATTRIBUTE_INVERSE;
        if (flags & GHOSTTY_STYLE_FLAG_INVISIBLE) attributes |= SNAPSHOT_ATTRIBUTE_INVISIBLE;
        if (flags & GHOSTTY_STYLE_FLAG_STRIKETHROUGH) attributes |= SNAPSHOT_ATTRIBUTE_STRIKETHROUGH;
        if (flags & GHOSTTY_STYLE_FLAG_BLINK) attributes |= SNAPSHOT_ATTRIBUTE_BLINK;
        put8(out, at + SNAPSHOT_CELL_ATTRIBUTES, attributes);
        put8(out, at + SNAPSHOT_CELL_UNDERLINE, underline_of(GHOSTTY_STYLE_UNDERLINE(style)));
    }
    return true;
}

// 行迭代器当前那一行的记录：行号加 columns 个格子，render state 里不够的格子补空白。内存不够时返回假。
static bool write_row(Vt *vt, Out *out, int32_t y, uint16_t columns) {
    size_t at = out->len;
    out->len += SNAPSHOT_ROW_HEADER_SIZE + (size_t)columns * SNAPSHOT_CELL_SIZE;
    put32(out, at + SNAPSHOT_ROW_Y, y);
    size_t cells_at = at + SNAPSHOT_ROW_HEADER_SIZE;
    uint16_t x = 0;
    GhosttyRenderStateRowCells cells = vt->cells;
    if (ghostty_render_state_row_get(vt->rows, GHOSTTY_RENDER_STATE_ROW_DATA_CELLS, &cells) == GHOSTTY_SUCCESS) {
        for (; x < columns && ghostty_render_state_row_cells_next(vt->cells); x++) {
            if (!write_cell(vt, out, cells_at + (size_t)x * SNAPSHOT_CELL_SIZE)) {
                return false;
            }
        }
    }
    for (; x < columns; x++) {
        put_zero(out, cells_at + (size_t)x * SNAPSHOT_CELL_SIZE, SNAPSHOT_CELL_SIZE);
    }
    return true;
}

/** `write_images` 用 `ghostty_render_state_image_get_multi` 一次取齐的字段，顺序同 `image_keys`。 */
typedef struct {
    GhosttyImageProtocol protocol;
    uint32_t id;
    uint64_t generation;
    GhosttyImageLayer layer;
    int32_t z;
    int32_t column;
    int32_t row;
    uint32_t offset_x;
    uint32_t offset_y;
    uint32_t width;
    uint32_t height;
    uint32_t source_x;
    uint32_t source_y;
    uint32_t source_width;
    uint32_t source_height;
} ImagePlacement;

static const GhosttyRenderStateImageData image_keys[] = {
    GHOSTTY_RENDER_STATE_IMAGE_DATA_PROTOCOL,       GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_ID,
    GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_GENERATION, GHOSTTY_RENDER_STATE_IMAGE_DATA_LAYER,
    GHOSTTY_RENDER_STATE_IMAGE_DATA_Z,              GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_COL,
    GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_ROW,   GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_X,
    GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_Y,  GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_WIDTH,
    GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_HEIGHT,   GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_X,
    GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_Y,       GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_WIDTH,
    GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_HEIGHT,
};

// render state 里的 Kitty 图片按读到的先后写成记录，返回写了几个。别的协议的图片和这个版本还不认识的层都跳过。
static int32_t write_images(Vt *vt, Out *out) {
    GhosttyRenderStateImageIterator iterator = vt->images;
    if (ghostty_render_state_get(vt->render_state, GHOSTTY_RENDER_STATE_DATA_IMAGE_ITERATOR, &iterator) !=
        GHOSTTY_SUCCESS) {
        return 0;
    }
    int32_t count = 0;
    while (ghostty_render_state_image_iterator_next(vt->images)) {
        ImagePlacement p = {.protocol = GHOSTTY_IMAGE_PROTOCOL_KITTY, .layer = GHOSTTY_IMAGE_LAYER_ABOVE_TEXT};
        void *values[] = {
            &p.protocol, &p.id,       &p.generation, &p.layer,    &p.z,        &p.column,       &p.row,
            &p.offset_x, &p.offset_y, &p.width,      &p.height,   &p.source_x, &p.source_y,     &p.source_width,
            &p.source_height,
        };
        if (ghostty_render_state_image_get_multi(vt->images, sizeof(image_keys) / sizeof(image_keys[0]), image_keys,
                                                 values, NULL) != GHOSTTY_SUCCESS ||
            p.protocol != GHOSTTY_IMAGE_PROTOCOL_KITTY) {
            continue;
        }
        int32_t layer;
        switch (p.layer) {
        case GHOSTTY_IMAGE_LAYER_BELOW_BACKGROUND: layer = SNAPSHOT_LAYER_BELOW_BACKGROUND; break;
        case GHOSTTY_IMAGE_LAYER_BELOW_TEXT: layer = SNAPSHOT_LAYER_BELOW_TEXT; break;
        case GHOSTTY_IMAGE_LAYER_ABOVE_TEXT: layer = SNAPSHOT_LAYER_ABOVE_TEXT; break;
        default: continue;
        }
        size_t at = out->len;
        out->len += SNAPSHOT_IMAGE_SIZE;
        put_zero(out, at, SNAPSHOT_IMAGE_SIZE);
        put32(out, at + SNAPSHOT_IMAGE_ID, (int32_t)p.id);
        put32(out, at + SNAPSHOT_IMAGE_LAYER, layer);
        put32(out, at + SNAPSHOT_IMAGE_Z, p.z);
        put32(out, at + SNAPSHOT_IMAGE_COLUMN, p.column);
        put32(out, at + SNAPSHOT_IMAGE_ROW, p.row);
        put32(out, at + SNAPSHOT_IMAGE_OFFSET_X, (int32_t)p.offset_x);
        put32(out, at + SNAPSHOT_IMAGE_OFFSET_Y, (int32_t)p.offset_y);
        put32(out, at + SNAPSHOT_IMAGE_WIDTH, (int32_t)p.width);
        put32(out, at + SNAPSHOT_IMAGE_HEIGHT, (int32_t)p.height);
        put32(out, at + SNAPSHOT_IMAGE_SOURCE_X, (int32_t)p.source_x);
        put32(out, at + SNAPSHOT_IMAGE_SOURCE_Y, (int32_t)p.source_y);
        put32(out, at + SNAPSHOT_IMAGE_SOURCE_WIDTH, (int32_t)p.source_width);
        put32(out, at + SNAPSHOT_IMAGE_SOURCE_HEIGHT, (int32_t)p.source_height);
        put64(out, at + SNAPSHOT_IMAGE_GENERATION, (int64_t)p.generation);
        count++;
    }
    return count;
}

static GhosttyKittyGraphics kitty_graphics(Vt *vt) {
    GhosttyKittyGraphics graphics = NULL;
    if (ghostty_terminal_get(vt->terminal, GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS, &graphics) != GHOSTTY_SUCCESS) {
        return NULL;
    }
    return graphics;
}

// 让在播的图片动画走到该放的那一帧，返回下一帧离现在多少毫秒；没有在播的为 -1。动画的时钟用 CLOCK_MONOTONIC，
// libghostty 要求同一份 VT 一直用同一个零点。
static int64_t advance_animations(Vt *vt) {
    GhosttyKittyGraphics graphics = kitty_graphics(vt);
    if (graphics == NULL) {
        return -1;
    }
    uint64_t now = (uint64_t)(monotonic_ns() / 1000000);
    uint64_t next = 0;
    if (ghostty_kitty_graphics_animation_tick(graphics, now, &next) != GHOSTTY_SUCCESS) {
        return -1;
    }
    return next > now ? (int64_t)(next - now) : 0;
}

// 按 render state 把要重画的行、光标、颜色和变了的图片列表写进 buffer（direct ByteBuffer），布局是 SNAPSHOT_*
// 那组常量。frame_* 是调用方手里那一帧现在的样子（颜色是 0xRRGGBB，没有光标色为 -1），据此判断
// 要不要整屏重画、视口上面那一行变没变。返回写了多少字节；render state 更新失败时返回 0；buffer 不够大时返回
// 负的所需字节数，这时什么都没消耗，调用方扩容后带着 retry 再调一次，不再推动画、不再更新 render state。
JNIEXPORT jint JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeSnapshot(
    JNIEnv *env, jclass cls, jlong handle, jobject buffer, jboolean retry, jint frame_columns, jint frame_rows,
    jint frame_background, jint frame_foreground, jint frame_cursor_color, jint frame_above_columns) {
    (void)cls;
    Vt *vt = vt_of(handle);
    Out out = {.base = (*env)->GetDirectBufferAddress(env, buffer), .cap = 0, .len = SNAPSHOT_HEADER_SIZE};
    if (out.base != NULL) {
        jlong capacity = (*env)->GetDirectBufferCapacity(env, buffer);
        out.cap = capacity > 0 ? (size_t)capacity : 0;
    }
    if (!retry) {
        vt->next_image_frame_ms = advance_animations(vt);
        if (ghostty_render_state_update(vt->render_state, vt->terminal) != GHOSTTY_SUCCESS) {
            return 0;
        }
    }
    GhosttyRenderState state = vt->render_state;
    GhosttyRenderStateDirty dirty = GHOSTTY_RENDER_STATE_DIRTY_FALSE;
    uint16_t columns = 0;
    uint16_t rows = 0;
    ghostty_render_state_get(state, GHOSTTY_RENDER_STATE_DATA_DIRTY, &dirty);
    ghostty_render_state_get(state, GHOSTTY_RENDER_STATE_DATA_COLS, &columns);
    ghostty_render_state_get(state, GHOSTTY_RENDER_STATE_DATA_ROWS, &rows);
    GhosttyRenderStateColors colors = GHOSTTY_INIT_SIZED(GhosttyRenderStateColors);
    ghostty_render_state_get(state, GHOSTTY_RENDER_STATE_DATA_COLORS, &colors);
    int32_t background = rgb_of(colors.background);
    int32_t foreground = rgb_of(colors.foreground);
    int32_t cursor_color = colors.cursor_has_value ? rgb_of(colors.cursor) : -1;
    bool full = dirty == GHOSTTY_RENDER_STATE_DIRTY_FULL || columns != frame_columns || rows != frame_rows ||
                background != frame_background || foreground != frame_foreground ||
                cursor_color != frame_cursor_color;

    // 视口在历史最顶上时取不到上面那一行，也就不能往下错开。
    GhosttyRenderStateOverscan captured = {0};
    ghostty_render_state_get(state, GHOSTTY_RENDER_STATE_DATA_OVERSCAN, &captured);
    int32_t above_columns = captured.above > 0 ? columns : 0;
    bool above_changed = above_columns != frame_above_columns;

    vt->graphemes_len = 0;
    int32_t row_count = 0;
    if (dirty != GHOSTTY_RENDER_STATE_DIRTY_FALSE || full || above_changed) {
        GhosttyRenderStateRowIterator iterator = vt->rows;
        ghostty_render_state_get(state, GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, &iterator);
        while (ghostty_render_state_row_iterator_next(vt->rows)) {
            int32_t y = 0;
            bool row_dirty = false;
            ghostty_render_state_row_get(vt->rows, GHOSTTY_RENDER_STATE_ROW_DATA_VIEWPORT_Y, &y);
            ghostty_render_state_row_get(vt->rows, GHOSTTY_RENDER_STATE_ROW_DATA_DIRTY, &row_dirty);
            bool write = false;
            if (y == -1 && above_columns > 0) {
                write = full || row_dirty || above_changed;
                above_changed = above_changed || write;
            } else if (y >= 0 && y < rows) {
                write = full || row_dirty;
            }
            if (write) {
                if (!write_row(vt, &out, y, columns)) {
                    return 0;
                }
                row_count++;
            }
        }
    }

    GhosttyRenderStateCursor cursor = GHOSTTY_INIT_SIZED(GhosttyRenderStateCursor);
    bool has_cursor = ghostty_render_state_get(state, GHOSTTY_RENDER_STATE_DATA_CURSOR, &cursor) == GHOSTTY_SUCCESS &&
                      cursor.visible && cursor.viewport_has_value;

    uint64_t image_list = 0;
    ghostty_render_state_get(state, GHOSTTY_RENDER_STATE_DATA_IMAGE_PLACEMENTS_GENERATION, &image_list);
    // 新的一帧（`full`）里还没有图片，列表没变也要读。
    bool images_changed = full || !vt->has_image_list || vt->image_list != image_list;
    out.len = (out.len + 7) & ~(size_t)7;
    size_t images_offset = out.len;
    int32_t image_count = images_changed ? write_images(vt, &out) : 0;

    size_t graphemes_offset = out.len;
    size_t graphemes_bytes = vt->graphemes_len * sizeof(uint32_t);
    if (graphemes_offset + graphemes_bytes <= out.cap && graphemes_bytes > 0) {
        memcpy(out.base + graphemes_offset, vt->graphemes, graphemes_bytes);
    }
    out.len += graphemes_bytes;
    if (out.len > INT32_MAX) {
        return 0;
    }
    if (out.len > out.cap) {
        return -(jint)out.len;
    }

    int32_t flags = 0;
    if (full) flags |= SNAPSHOT_FLAG_FULL;
    if (above_changed) flags |= SNAPSHOT_FLAG_ABOVE_CHANGED;
    if (images_changed) flags |= SNAPSHOT_FLAG_IMAGES_CHANGED;
    if (cursor_color >= 0) flags |= SNAPSHOT_FLAG_CURSOR_COLOR;
    if (vt->next_image_frame_ms >= 0) flags |= SNAPSHOT_FLAG_NEXT_IMAGE_FRAME;
    int32_t cursor_shape = SNAPSHOT_CURSOR_BLOCK;
    if (has_cursor) {
        flags |= SNAPSHOT_FLAG_CURSOR;
        if (cursor.blinking) flags |= SNAPSHOT_FLAG_CURSOR_BLINKING;
        if (cursor.wide_tail) flags |= SNAPSHOT_FLAG_CURSOR_WIDE_TAIL;
        switch (cursor.visual_style) {
        case GHOSTTY_CURSOR_STYLE_BAR: cursor_shape = SNAPSHOT_CURSOR_BAR; break;
        case GHOSTTY_CURSOR_STYLE_UNDERLINE: cursor_shape = SNAPSHOT_CURSOR_UNDERLINE; break;
        case GHOSTTY_CURSOR_STYLE_BLOCK_HOLLOW: cursor_shape = SNAPSHOT_CURSOR_BLOCK_HOLLOW; break;
        default: break;
        }
    }
    put_zero(&out, 0, SNAPSHOT_HEADER_SIZE);
    put32(&out, SNAPSHOT_FLAGS, flags);
    put32(&out, SNAPSHOT_COLUMNS, columns);
    put32(&out, SNAPSHOT_ROWS, rows);
    put32(&out, SNAPSHOT_BACKGROUND, background);
    put32(&out, SNAPSHOT_FOREGROUND, foreground);
    put32(&out, SNAPSHOT_CURSOR_COLOR, cursor_color >= 0 ? cursor_color : 0);
    put32(&out, SNAPSHOT_ABOVE_COLUMNS, above_columns);
    put32(&out, SNAPSHOT_CURSOR_COLUMN, has_cursor ? cursor.viewport_x : 0);
    put32(&out, SNAPSHOT_CURSOR_ROW, has_cursor ? cursor.viewport_y : 0);
    put32(&out, SNAPSHOT_CURSOR_SHAPE, cursor_shape);
    put32(&out, SNAPSHOT_ROW_COUNT, row_count);
    put32(&out, SNAPSHOT_IMAGE_COUNT, image_count);
    put32(&out, SNAPSHOT_IMAGES_OFFSET, (int32_t)images_offset);
    put32(&out, SNAPSHOT_GRAPHEMES_OFFSET, (int32_t)graphemes_offset);
    put64(&out, SNAPSHOT_NEXT_IMAGE_FRAME_MS, vt->next_image_frame_ms >= 0 ? vt->next_image_frame_ms : 0);

    if (images_changed) {
        vt->has_image_list = true;
        vt->image_list = image_list;
    }
    ghostty_render_state_clean(state);
    return (jint)out.len;
}

// 一块 Kitty 图片现在的像素，转成不预乘的 RGBA；size 里写宽和高。图片已经换了版本（不是 generation）、删了、
// 格式不认识时返回 NULL。
JNIEXPORT jbyteArray JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeImagePixels(JNIEnv *env, jclass cls,
                                                                                     jlong handle, jint id,
                                                                                     jlong generation,
                                                                                     jintArray size) {
    (void)cls;
    GhosttyKittyGraphics graphics = kitty_graphics(vt_of(handle));
    GhosttyKittyGraphicsImage image = graphics != NULL ? ghostty_kitty_graphics_image(graphics, (uint32_t)id) : NULL;
    if (image == NULL) {
        return NULL;
    }
    uint64_t current = 0;
    uint32_t width = 0;
    uint32_t height = 0;
    GhosttyKittyImageFormat format = GHOSTTY_KITTY_IMAGE_FORMAT_RGBA;
    GhosttyString stored = {0};
    if (ghostty_kitty_graphics_image_get(image, GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_GENERATION, &current) !=
            GHOSTTY_SUCCESS ||
        current != (uint64_t)generation ||
        ghostty_kitty_graphics_image_get(image, GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_WIDTH, &width) != GHOSTTY_SUCCESS ||
        ghostty_kitty_graphics_image_get(image, GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_HEIGHT, &height) !=
            GHOSTTY_SUCCESS ||
        ghostty_kitty_graphics_image_get(image, GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_FORMAT, &format) !=
            GHOSTTY_SUCCESS ||
        ghostty_kitty_graphics_image_get(image, GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_PIXELS, &stored) !=
            GHOSTTY_SUCCESS) {
        return NULL;
    }
    size_t channels;
    switch (format) {
    case GHOSTTY_KITTY_IMAGE_FORMAT_RGBA: channels = 4; break;
    case GHOSTTY_KITTY_IMAGE_FORMAT_RGB: channels = 3; break;
    case GHOSTTY_KITTY_IMAGE_FORMAT_GRAY_ALPHA: channels = 2; break;
    case GHOSTTY_KITTY_IMAGE_FORMAT_GRAY: channels = 1; break;
    default: return NULL;
    }
    size_t count = (size_t)width * height;
    if (count == 0 || count > INT32_MAX / 4 || stored.ptr == NULL || stored.len / channels != count) {
        return NULL;
    }
    uint8_t *rgba = malloc(count * 4);
    if (rgba == NULL) {
        return NULL;
    }
    const uint8_t *pixels = stored.ptr;
    for (size_t i = 0; i < count; i++) {
        const uint8_t *p = pixels + i * channels;
        uint8_t *q = rgba + i * 4;
        switch (channels) {
        case 4: memcpy(q, p, 4); break;
        case 3: q[0] = p[0], q[1] = p[1], q[2] = p[2], q[3] = 255; break;
        case 2: q[0] = q[1] = q[2] = p[0], q[3] = p[1]; break;
        default: q[0] = q[1] = q[2] = p[0], q[3] = 255; break;
        }
    }
    jbyteArray array = byte_array(env, rgba, count * 4);
    free(rgba);
    jint dimensions[2] = {(jint)width, (jint)height};
    (*env)->SetIntArrayRegion(env, size, 0, 2, dimensions);
    return array;
}

// MARK: 输入编码

// 按这份 VT 当前的模式编码一次按下的键。key、mods、consumed_mods、option_as_alt 是 libghostty 的取值，
// unshifted 是不按修饰键时的码点（没有为 0），text 是打出的 UTF-8（没有为 NULL）。编不出时返回空数组。
JNIEXPORT jbyteArray JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeEncodeKey(
    JNIEnv *env, jclass cls, jlong handle, jint key, jint mods, jint consumed_mods, jint unshifted, jbyteArray text,
    jint option_as_alt) {
    (void)cls;
    Vt *vt = vt_of(handle);
    ghostty_key_encoder_set_from_terminal(vt->key_encoder, vt->terminal);
    GhosttyOptionAsAlt option = (GhosttyOptionAsAlt)option_as_alt;
    ghostty_key_encoder_set(vt->key_encoder, GHOSTTY_KEY_ENCODER_OPT_MACOS_OPTION_AS_ALT, &option);
    ghostty_key_event_set_action(vt->key_event, GHOSTTY_KEY_ACTION_PRESS);
    ghostty_key_event_set_key(vt->key_event, (GhosttyKey)key);
    ghostty_key_event_set_mods(vt->key_event, (GhosttyMods)mods);
    ghostty_key_event_set_consumed_mods(vt->key_event, (GhosttyMods)consumed_mods);
    ghostty_key_event_set_composing(vt->key_event, false);
    ghostty_key_event_set_unshifted_codepoint(vt->key_event, (uint32_t)unshifted);
    // 文字只在编码期间借给 libghostty。
    jbyte *utf8 = text != NULL ? (*env)->GetByteArrayElements(env, text, NULL) : NULL;
    GhosttyString string = {0};
    if (utf8 != NULL) {
        string.ptr = (const uint8_t *)utf8;
        string.len = (size_t)(*env)->GetArrayLength(env, text);
    }
    ghostty_key_event_set_utf8(vt->key_event, string);
    uint8_t stack[128];
    uint8_t *output = stack;
    size_t written = 0;
    GhosttyResult result = ghostty_key_encoder_encode(vt->key_encoder, vt->key_event, output, sizeof(stack), &written);
    if (result == GHOSTTY_OUT_OF_SPACE) {
        output = malloc(written);
        result = output != NULL ? ghostty_key_encoder_encode(vt->key_encoder, vt->key_event, output, written, &written)
                                : GHOSTTY_OUT_OF_MEMORY;
    }
    ghostty_key_event_set_utf8(vt->key_event, (GhosttyString){0});
    if (utf8 != NULL) {
        (*env)->ReleaseByteArrayElements(env, text, utf8, JNI_ABORT);
    }
    jbyteArray bytes = byte_array(env, output, result == GHOSTTY_SUCCESS ? written : 0);
    if (output != stack) {
        free(output);
    }
    return bytes;
}

// 按程序要的鼠标上报格式编码一个事件。action、button 是 libghostty 的取值，位置落在第 row 行第 column 列
// 格子的正中，像素坐标换算成格子时不会因为舍入落到隔壁。编码失败时返回 NULL。
JNIEXPORT jbyteArray JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeEncodeMouse(
    JNIEnv *env, jclass cls, jlong handle, jint action, jint button, jint column, jint row, jint columns, jint rows,
    jint cell_width, jint cell_height) {
    (void)cls;
    Vt *vt = vt_of(handle);
    GhosttyMouseEncoderSize size = GHOSTTY_INIT_SIZED(GhosttyMouseEncoderSize);
    size.screen_width = (uint32_t)columns * (uint32_t)cell_width;
    size.screen_height = (uint32_t)rows * (uint32_t)cell_height;
    size.cell_width = (uint32_t)cell_width;
    size.cell_height = (uint32_t)cell_height;
    ghostty_mouse_encoder_set_from_terminal(vt->mouse_encoder, vt->terminal);
    ghostty_mouse_encoder_set(vt->mouse_encoder, GHOSTTY_MOUSE_ENCODER_OPT_SIZE, &size);
    bool pressed = action != GHOSTTY_MOUSE_ACTION_RELEASE;
    ghostty_mouse_encoder_set(vt->mouse_encoder, GHOSTTY_MOUSE_ENCODER_OPT_ANY_BUTTON_PRESSED, &pressed);
    ghostty_mouse_event_set_action(vt->mouse_event, (GhosttyMouseAction)action);
    ghostty_mouse_event_set_button(vt->mouse_event, (GhosttyMouseButton)button);
    ghostty_mouse_event_set_mods(vt->mouse_event, 0);
    GhosttyMousePosition position = {
        .x = ((float)column + 0.5f) * (float)cell_width,
        .y = ((float)row + 0.5f) * (float)cell_height,
    };
    ghostty_mouse_event_set_position(vt->mouse_event, position);
    uint8_t output[64];
    size_t written = 0;
    if (ghostty_mouse_encoder_encode(vt->mouse_encoder, vt->mouse_event, output, sizeof(output), &written) !=
        GHOSTTY_SUCCESS) {
        return NULL;
    }
    return byte_array(env, output, written);
}

// 粘贴的字节：去掉不安全的控制字符，bracketed 时套上括号，否则换行换成回车。编不出时返回空数组。
JNIEXPORT jbyteArray JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeEncodePaste(JNIEnv *env, jclass cls,
                                                                                     jbyteArray text,
                                                                                     jboolean bracketed) {
    (void)cls;
    jsize length = (*env)->GetArrayLength(env, text);
    char *data = malloc((size_t)length + 1);
    if (data == NULL) {
        return byte_array(env, NULL, 0);
    }
    (*env)->GetByteArrayRegion(env, text, 0, length, (jbyte *)data);
    size_t capacity = (size_t)length + 16;
    uint8_t *output = malloc(capacity);
    size_t written = 0;
    GhosttyResult result = output != NULL
                               ? ghostty_paste_encode(data, (size_t)length, bracketed, output, capacity, &written)
                               : GHOSTTY_OUT_OF_MEMORY;
    if (result == GHOSTTY_OUT_OF_SPACE) {
        // libghostty 改过 data，原样重编得到同样的结果。
        free(output);
        capacity = written;
        output = malloc(capacity);
        result = output != NULL ? ghostty_paste_encode(data, (size_t)length, bracketed, output, capacity, &written)
                                : GHOSTTY_OUT_OF_MEMORY;
    }
    jbyteArray bytes = byte_array(env, output, result == GHOSTTY_SUCCESS ? written : 0);
    free(output);
    free(data);
    return bytes;
}

// MARK: 和 Kotlin 对账

static const jint layout_signature[] = {
    SNAPSHOT_VERSION,
    SNAPSHOT_HEADER_SIZE,
    SNAPSHOT_FLAGS,
    SNAPSHOT_COLUMNS,
    SNAPSHOT_ROWS,
    SNAPSHOT_BACKGROUND,
    SNAPSHOT_FOREGROUND,
    SNAPSHOT_CURSOR_COLOR,
    SNAPSHOT_ABOVE_COLUMNS,
    SNAPSHOT_CURSOR_COLUMN,
    SNAPSHOT_CURSOR_ROW,
    SNAPSHOT_CURSOR_SHAPE,
    SNAPSHOT_ROW_COUNT,
    SNAPSHOT_IMAGE_COUNT,
    SNAPSHOT_IMAGES_OFFSET,
    SNAPSHOT_GRAPHEMES_OFFSET,
    SNAPSHOT_NEXT_IMAGE_FRAME_MS,
    SNAPSHOT_FLAG_FULL,
    SNAPSHOT_FLAG_ABOVE_CHANGED,
    SNAPSHOT_FLAG_IMAGES_CHANGED,
    SNAPSHOT_FLAG_CURSOR,
    SNAPSHOT_FLAG_CURSOR_BLINKING,
    SNAPSHOT_FLAG_CURSOR_WIDE_TAIL,
    SNAPSHOT_FLAG_CURSOR_COLOR,
    SNAPSHOT_FLAG_NEXT_IMAGE_FRAME,
    SNAPSHOT_ROW_Y,
    SNAPSHOT_ROW_HEADER_SIZE,
    SNAPSHOT_CELL_TEXT,
    SNAPSHOT_CELL_FOREGROUND,
    SNAPSHOT_CELL_BACKGROUND,
    SNAPSHOT_CELL_ATTRIBUTES,
    SNAPSHOT_CELL_UNDERLINE,
    SNAPSHOT_CELL_WIDTH,
    SNAPSHOT_CELL_SIZE,
    SNAPSHOT_CELL_TEXT_GRAPHEME,
    SNAPSHOT_COLOR_SET,
    SNAPSHOT_ATTRIBUTE_BOLD,
    SNAPSHOT_ATTRIBUTE_ITALIC,
    SNAPSHOT_ATTRIBUTE_FAINT,
    SNAPSHOT_ATTRIBUTE_INVERSE,
    SNAPSHOT_ATTRIBUTE_INVISIBLE,
    SNAPSHOT_ATTRIBUTE_STRIKETHROUGH,
    SNAPSHOT_ATTRIBUTE_BLINK,
    SNAPSHOT_UNDERLINE_NONE,
    SNAPSHOT_UNDERLINE_SINGLE,
    SNAPSHOT_UNDERLINE_DOUBLE,
    SNAPSHOT_UNDERLINE_CURLY,
    SNAPSHOT_UNDERLINE_DOTTED,
    SNAPSHOT_UNDERLINE_DASHED,
    SNAPSHOT_WIDTH_NARROW,
    SNAPSHOT_WIDTH_WIDE,
    SNAPSHOT_WIDTH_SPACER_TAIL,
    SNAPSHOT_WIDTH_SPACER_HEAD,
    SNAPSHOT_CURSOR_BLOCK,
    SNAPSHOT_CURSOR_BLOCK_HOLLOW,
    SNAPSHOT_CURSOR_BAR,
    SNAPSHOT_CURSOR_UNDERLINE,
    SNAPSHOT_IMAGE_ID,
    SNAPSHOT_IMAGE_LAYER,
    SNAPSHOT_IMAGE_Z,
    SNAPSHOT_IMAGE_COLUMN,
    SNAPSHOT_IMAGE_ROW,
    SNAPSHOT_IMAGE_OFFSET_X,
    SNAPSHOT_IMAGE_OFFSET_Y,
    SNAPSHOT_IMAGE_WIDTH,
    SNAPSHOT_IMAGE_HEIGHT,
    SNAPSHOT_IMAGE_SOURCE_X,
    SNAPSHOT_IMAGE_SOURCE_Y,
    SNAPSHOT_IMAGE_SOURCE_WIDTH,
    SNAPSHOT_IMAGE_SOURCE_HEIGHT,
    SNAPSHOT_IMAGE_GENERATION,
    SNAPSHOT_IMAGE_SIZE,
    SNAPSHOT_LAYER_BELOW_BACKGROUND,
    SNAPSHOT_LAYER_BELOW_TEXT,
    SNAPSHOT_LAYER_ABOVE_TEXT,
};

// Kotlin 的 GhosttyAbi 照抄的 libghostty 取值，顺序和 GhosttyAbi.signature 一样。
static const jint ghostty_constants[] = {
    GHOSTTY_KEY_UNIDENTIFIED,
    GHOSTTY_KEY_A,
    GHOSTTY_KEY_Z,
    GHOSTTY_KEY_DIGIT_0,
    GHOSTTY_KEY_DIGIT_9,
    GHOSTTY_KEY_F1,
    GHOSTTY_KEY_F24,
    GHOSTTY_KEY_SPACE,
    GHOSTTY_KEY_ENTER,
    GHOSTTY_KEY_TAB,
    GHOSTTY_KEY_BACKSPACE,
    GHOSTTY_KEY_ESCAPE,
    GHOSTTY_KEY_INSERT,
    GHOSTTY_KEY_DELETE,
    GHOSTTY_KEY_HOME,
    GHOSTTY_KEY_END,
    GHOSTTY_KEY_PAGE_UP,
    GHOSTTY_KEY_PAGE_DOWN,
    GHOSTTY_KEY_ARROW_UP,
    GHOSTTY_KEY_ARROW_DOWN,
    GHOSTTY_KEY_ARROW_LEFT,
    GHOSTTY_KEY_ARROW_RIGHT,
    GHOSTTY_KEY_BACKQUOTE,
    GHOSTTY_KEY_BACKSLASH,
    GHOSTTY_KEY_BRACKET_LEFT,
    GHOSTTY_KEY_BRACKET_RIGHT,
    GHOSTTY_KEY_COMMA,
    GHOSTTY_KEY_EQUAL,
    GHOSTTY_KEY_MINUS,
    GHOSTTY_KEY_PERIOD,
    GHOSTTY_KEY_QUOTE,
    GHOSTTY_KEY_SEMICOLON,
    GHOSTTY_KEY_SLASH,
    GHOSTTY_MODS_SHIFT,
    GHOSTTY_MODS_CTRL,
    GHOSTTY_MODS_ALT,
    GHOSTTY_MODS_SUPER,
    GHOSTTY_MODS_ALT_SIDE,
    GHOSTTY_OPTION_AS_ALT_FALSE,
    GHOSTTY_OPTION_AS_ALT_TRUE,
    GHOSTTY_OPTION_AS_ALT_LEFT,
    GHOSTTY_OPTION_AS_ALT_RIGHT,
    GHOSTTY_CURSOR_STYLE_BLOCK,
    GHOSTTY_CURSOR_STYLE_BLOCK_HOLLOW,
    GHOSTTY_CURSOR_STYLE_BAR,
    GHOSTTY_CURSOR_STYLE_UNDERLINE,
    GHOSTTY_MOUSE_ACTION_PRESS,
    GHOSTTY_MOUSE_ACTION_RELEASE,
    GHOSTTY_MOUSE_BUTTON_LEFT,
    GHOSTTY_MOUSE_BUTTON_FOUR,
    GHOSTTY_MOUSE_BUTTON_FIVE,
};

static jintArray int_array(JNIEnv *env, const jint *values, jsize count) {
    jintArray array = (*env)->NewIntArray(env, count);
    if (array != NULL) {
        (*env)->SetIntArrayRegion(env, array, 0, count, values);
    }
    return array;
}

JNIEXPORT jintArray JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeLayout(JNIEnv *env, jclass cls) {
    (void)cls;
    return int_array(env, layout_signature, sizeof(layout_signature) / sizeof(layout_signature[0]));
}

JNIEXPORT jintArray JNICALL Java_dev_runode_mobile_terminal_VtJni_nativeGhosttyConstants(JNIEnv *env, jclass cls) {
    (void)cls;
    return int_array(env, ghostty_constants, sizeof(ghostty_constants) / sizeof(ghostty_constants[0]));
}
