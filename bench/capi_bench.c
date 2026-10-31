// Benchmarks zz's hot paths through the libghostty-vt C API.
//
// Build this file once against each engine (the Rust static library and
// zz's Zig libghostty-vt) and compare them with bench/capi_compare.py.
// Inputs are generated here from fixed seeds, so both builds see the same
// bytes. Each scenario prints one JSON line with the CPU time of each
// timed repetition (and user-space instructions on Linux).
//
//   capi_bench list
//   capi_bench <scenario> [reps]

#define _GNU_SOURCE
#include <ghostty/vt.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#if defined(__linux__)
#include <linux/perf_event.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <unistd.h>
#endif

#define OK(expr)                                                              \
  do {                                                                        \
    GhosttyResult r_ = (expr);                                                \
    if (r_ != GHOSTTY_SUCCESS) {                                              \
      fprintf(stderr, "%s:%d: result %d\n", __FILE__, __LINE__, (int)r_);   \
      abort();                                                                \
    }                                                                         \
  } while (0)

// ---------------------------------------------------------------- timing

static double cpu_now(void) {
  struct timespec t;
  clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &t);
  return t.tv_sec * 1e9 + t.tv_nsec;
}

#if defined(__linux__)
static int instr_fd = -1;
#endif

static void instr_open(void) {
#if defined(__linux__)
  struct perf_event_attr a;
  memset(&a, 0, sizeof a);
  a.size = sizeof a;
  a.type = PERF_TYPE_HARDWARE;
  a.config = PERF_COUNT_HW_INSTRUCTIONS;
  a.disabled = 1;
  a.exclude_kernel = 1;
  a.exclude_hv = 1;
  instr_fd = (int)syscall(SYS_perf_event_open, &a, 0, -1, -1, 0);
#endif
}

static void instr_start(void) {
#if defined(__linux__)
  if (instr_fd < 0) return;
  ioctl(instr_fd, PERF_EVENT_IOC_RESET, 0);
  ioctl(instr_fd, PERF_EVENT_IOC_ENABLE, 0);
#endif
}

static long long instr_stop(void) {
#if defined(__linux__)
  if (instr_fd < 0) return -1;
  ioctl(instr_fd, PERF_EVENT_IOC_DISABLE, 0);
  long long n = 0;
  if (read(instr_fd, &n, sizeof n) != sizeof n) return -1;
  return n;
#else
  return -1;
#endif
}

// ---------------------------------------------------------------- inputs

typedef struct {
  uint8_t *p;
  size_t len, cap;
} Buf;

static void put(Buf *b, const void *s, size_t n) {
  if (b->len + n > b->cap) {
    b->cap = (b->len + n) * 2 + 4096;
    b->p = realloc(b->p, b->cap);
    if (!b->p) abort();
  }
  memcpy(b->p + b->len, s, n);
  b->len += n;
}
static void puts_(Buf *b, const char *s) { put(b, s, strlen(s)); }
static void putf(Buf *b, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
#include <stdarg.h>
static void putf(Buf *b, const char *fmt, ...) {
  char tmp[256];
  va_list ap;
  va_start(ap, fmt);
  int n = vsnprintf(tmp, sizeof tmp, fmt, ap);
  va_end(ap);
  put(b, tmp, (size_t)n);
}
static void put_cp(Buf *b, uint32_t c) {
  uint8_t o[4];
  size_t n;
  if (c < 0x80) { o[0] = (uint8_t)c; n = 1; }
  else if (c < 0x800) { o[0] = 0xC0 | (c >> 6); o[1] = 0x80 | (c & 63); n = 2; }
  else if (c < 0x10000) { o[0] = 0xE0 | (c >> 12); o[1] = 0x80 | ((c >> 6) & 63); o[2] = 0x80 | (c & 63); n = 3; }
  else { o[0] = 0xF0 | (c >> 18); o[1] = 0x80 | ((c >> 12) & 63); o[2] = 0x80 | ((c >> 6) & 63); o[3] = 0x80 | (c & 63); n = 4; }
  put(b, o, n);
}

static uint64_t rng = 0x9E3779B97F4A7C15ull;
static uint32_t rnd(uint32_t n) {
  rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17;
  return (uint32_t)(rng % n);
}

typedef enum { IN_ASCII, IN_UTF8, IN_CJK, IN_STYLED, IN_LINKS, IN_CSI, IN_WRAPPED } Input;

// About `size` bytes of output of one kind, ending at a line boundary.
static Buf make_input(Input kind, size_t size) {
  Buf b = {0};
  rng = 0x9E3779B97F4A7C15ull + kind;
  static const uint32_t latin[] = {0xE9, 0xE8, 0xEA, 0xF1, 0xFC, 0xE7, 0x101, 0x15F, 0x3B1, 0x3B2, 0x436, 0x44F};
  static const uint32_t emoji[] = {0x1F600, 0x1F680, 0x1F44D, 0x2728, 0x1F525, 0x2705};
  unsigned line = 0;
  while (b.len < size) {
    switch (kind) {
    case IN_ASCII:
      for (int i = 0; i < 100; i++) { char c = (char)(' ' + rnd(95)); put(&b, &c, 1); }
      break;
    case IN_UTF8:
      for (int i = 0; i < 70; i++) {
        uint32_t k = rnd(100);
        if (k < 60) { char c = (char)('a' + rnd(26)); put(&b, &c, 1); }
        else if (k < 80) put_cp(&b, latin[rnd(12)]);
        else if (k < 92) put_cp(&b, 0x4E00 + rnd(0x5000));
        else if (k < 96) put_cp(&b, emoji[rnd(6)]);
        else { put_cp(&b, 'e'); put_cp(&b, 0x301); }
      }
      break;
    case IN_CJK:
      for (int i = 0; i < 55; i++) put_cp(&b, 0x4E00 + rnd(0x5000));
      break;
    case IN_STYLED:
      for (int i = 0; i < 12; i++) {
        switch (rnd(4)) {
        case 0: putf(&b, "\x1b[3%um", rnd(8)); break;
        case 1: putf(&b, "\x1b[38;5;%um", rnd(256)); break;
        case 2: putf(&b, "\x1b[38;2;%u;%u;%um", rnd(256), rnd(256), rnd(256)); break;
        default: puts_(&b, "\x1b[1;4m"); break;
        }
        for (int j = 0; j < 8; j++) { char c = (char)('a' + rnd(26)); put(&b, &c, 1); }
        puts_(&b, "\x1b[0m ");
      }
      break;
    case IN_LINKS:
      putf(&b, "\x1b]8;id=l%u;https://example.com/path/%u\x1b\\link %u\x1b]8;;\x1b\\ trailing text", line % 7, line % 13, line);
      break;
    case IN_CSI:
      // A full-screen app repainting: move, clear, style, text, repeat.
      putf(&b, "\x1b[%u;%uH\x1b[K\x1b[%um", 1 + rnd(40), 1 + rnd(60), 30 + rnd(8));
      for (int i = 0; i < 20; i++) { char c = (char)('a' + rnd(26)); put(&b, &c, 1); }
      puts_(&b, "\x1b[0m");
      if (line % 50 == 0) puts_(&b, "\x1b[2;39r\x1b[39;1H\n\x1b[r");
      continue;
    case IN_WRAPPED:
      for (int i = 0; i < 300; i++) { char c = (char)('a' + rnd(26)); put(&b, &c, 1); }
      break;
    }
    puts_(&b, "\r\n");
    line++;
  }
  return b;
}

// ---------------------------------------------------------------- engine helpers

static GhosttyTerminal term(uint16_t cols, uint16_t rows, size_t history) {
  GhosttyTerminal t = NULL;
  OK(ghostty_terminal_new(NULL, &t, cols, rows));
  OK(ghostty_terminal_set(t, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES, &history));
  return t;
}

static void write_all(GhosttyTerminal t, const Buf *b, size_t chunk) {
  for (size_t i = 0; i < b->len; i += chunk) {
    size_t n = b->len - i < chunk ? b->len - i : chunk;
    ghostty_terminal_vt_write(t, b->p + i, n);
  }
}

// Reusable output buffers for row copies, grown on demand.
static GhosttyRenderStateRowCellsCopy cp;
static void copy_reset(void) {
  cp = (GhosttyRenderStateRowCellsCopy)GHOSTTY_INIT_SIZED(GhosttyRenderStateRowCellsCopy);
}
static uint64_t sink;

// One zz frame: update the render state and copy each dirty row in one call.
static void frame(GhosttyTerminal t, GhosttyRenderState rs, GhosttyRenderStateRowIterator ri,
                  GhosttyRenderStateRowCells ci, uint16_t cols) {
  OK(ghostty_render_state_update(rs, t));
  OK(ghostty_render_state_get(rs, GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, &ri));
  while (ghostty_render_state_row_iterator_next(ri)) {
    bool dirty = false;
    OK(ghostty_render_state_row_get(ri, GHOSTTY_RENDER_STATE_ROW_DATA_DIRTY, &dirty));
    if (!dirty) continue;
    OK(ghostty_render_state_row_get(ri, GHOSTTY_RENDER_STATE_ROW_DATA_CELLS, &ci));
    for (;;) {
      cp.flags = GHOSTTY_RENDER_STATE_ROW_CELLS_COPY_TRIM;
      GhosttyResult r = ghostty_render_state_row_cells_copy(ci, 0, cols, &cp);
      if (r == GHOSTTY_OUT_OF_SPACE) {
        cp.cells_cap = cp.cells_len * 2; cp.cells = realloc(cp.cells, cp.cells_cap * sizeof *cp.cells);
        cp.styles_cap = cp.styles_len * 2; cp.styles = realloc(cp.styles, cp.styles_cap * sizeof *cp.styles);
        cp.graphemes_cap = cp.graphemes_len * 2 + 1; cp.graphemes = realloc(cp.graphemes, cp.graphemes_cap * sizeof *cp.graphemes);
        cp.grapheme_bytes_cap = cp.grapheme_bytes_len * 2 + 1; cp.grapheme_bytes = realloc(cp.grapheme_bytes, cp.grapheme_bytes_cap);
        continue;
      }
      OK(r);
      sink += cp.cells_len;
      break;
    }
    bool clean = false;
    OK(ghostty_render_state_row_set(ri, GHOSTTY_RENDER_STATE_ROW_OPTION_DIRTY, &clean));
  }
  GhosttyRenderStateDirty d = GHOSTTY_RENDER_STATE_DIRTY_FALSE;
  OK(ghostty_render_state_set(rs, GHOSTTY_RENDER_STATE_OPTION_DIRTY, &d));
}

static size_t format_len(GhosttyTerminal t, GhosttyFormatterFormat emit) {
  GhosttyFormatterTerminalOptions o = GHOSTTY_INIT_SIZED(GhosttyFormatterTerminalOptions);
  o.emit = emit;
  o.extra = (GhosttyFormatterTerminalExtra)GHOSTTY_INIT_SIZED(GhosttyFormatterTerminalExtra);
  o.extra.screen = (GhosttyFormatterScreenExtra)GHOSTTY_INIT_SIZED(GhosttyFormatterScreenExtra);
  o.extra.screen.style = o.extra.screen.hyperlink = true;
  GhosttyFormatter f;
  OK(ghostty_formatter_terminal_new(NULL, &f, t, o));
  uint8_t *buf = NULL;
  size_t n = 0;
  OK(ghostty_formatter_format_alloc(f, NULL, &buf, &n));
  ghostty_free(NULL, buf, n);
  ghostty_formatter_free(f);
  return n;
}

// ---------------------------------------------------------------- scenarios

#define MB (1u << 20)
#define COLS 120
#define ROWS 40
#define HISTORY 10000

typedef struct {
  const char *name;
  Input input;
  size_t bytes;
  int kind;
} Scenario;

enum { K_WRITE, K_FRAME, K_IDLE, K_CLONE, K_CLONE_ONLY, K_FMT_PLAIN, K_FMT_VT, K_REFLOW, K_CHURN, K_COMPRESS, K_SCROLL };

static const Scenario scenarios[] = {
    {"write/ascii", IN_ASCII, 64 * MB, K_WRITE},
    {"write/utf8", IN_UTF8, 32 * MB, K_WRITE},
    {"write/cjk", IN_CJK, 32 * MB, K_WRITE},
    {"write/styled", IN_STYLED, 32 * MB, K_WRITE},
    {"write/links", IN_LINKS, 16 * MB, K_WRITE},
    {"write/csi", IN_CSI, 16 * MB, K_WRITE},
    {"frame/ascii", IN_ASCII, 16 * MB, K_FRAME},
    {"frame/utf8", IN_UTF8, 8 * MB, K_FRAME},
    {"frame/styled", IN_STYLED, 8 * MB, K_FRAME},
    {"frame/csi", IN_CSI, 8 * MB, K_FRAME},
    {"frame/idle", IN_STYLED, 64 * 1024, K_IDLE},
    {"clone/snapshot", IN_STYLED, 8 * MB, K_CLONE_ONLY},
    {"clone/history", IN_STYLED, 8 * MB, K_CLONE},
    {"format/plain", IN_STYLED, 4 * MB, K_FMT_PLAIN},
    {"format/vt", IN_STYLED, 4 * MB, K_FMT_VT},
    {"reflow/wrapped", IN_WRAPPED, 4 * MB, K_REFLOW},
    {"churn/new-free", IN_ASCII, 0, K_CHURN},
    {"compress/full", IN_STYLED, 32 * MB, K_COMPRESS},
    {"scroll/pages", IN_STYLED, 4 * MB, K_SCROLL},
};
#define NSCEN (sizeof scenarios / sizeof *scenarios)

// Runs one repetition. Untimed setup goes before `t0 = cpu_now()`; teardown
// after the timed work is not counted.
static void run(const Scenario *s, const Buf *in, double *ms, long long *instr) {
  double t0 = 0;
  GhosttyTerminal t = NULL;
  GhosttyRenderState rs = NULL;
  GhosttyRenderStateRowIterator ri = NULL;
  GhosttyRenderStateRowCells ci = NULL;
  switch (s->kind) {
  case K_WRITE:
    t = term(COLS, ROWS, HISTORY);
    t0 = cpu_now(); instr_start();
    write_all(t, in, 65536);
    break;
  case K_FRAME:
  case K_IDLE:
  case K_SCROLL:
    t = term(COLS, ROWS, HISTORY);
    OK(ghostty_render_state_new(NULL, &rs));
    OK(ghostty_render_state_row_iterator_new(NULL, &ri));
    OK(ghostty_render_state_row_cells_new(NULL, &ci));
    if (s->kind != K_FRAME) write_all(t, in, 65536);
    frame(t, rs, ri, ci, COLS);
    t0 = cpu_now(); instr_start();
    if (s->kind == K_FRAME) {
      // A frame after every 16 KiB of output, as a busy pane delivers it.
      for (size_t i = 0; i < in->len; i += 16384) {
        size_t n = in->len - i < 16384 ? in->len - i : 16384;
        ghostty_terminal_vt_write(t, in->p + i, n);
        frame(t, rs, ri, ci, COLS);
      }
    } else if (s->kind == K_IDLE) {
      for (int i = 0; i < 20000; i++) frame(t, rs, ri, ci, COLS);
    } else {
      // Page from the top of history to the bottom, drawing each page.
      for (int pass = 0; pass < 20; pass++) {
        GhosttyTerminalScrollViewport top = {.tag = GHOSTTY_SCROLL_VIEWPORT_TOP};
        ghostty_terminal_scroll_viewport(t, top);
        for (int i = 0; i < HISTORY / ROWS; i++) {
          GhosttyTerminalScrollViewport down = {.tag = GHOSTTY_SCROLL_VIEWPORT_DELTA, .value = {.delta = ROWS}};
          ghostty_terminal_scroll_viewport(t, down);
          frame(t, rs, ri, ci, COLS);
        }
      }
    }
    break;
  case K_CLONE:
  case K_CLONE_ONLY: {
    t = term(COLS, ROWS, HISTORY);
    write_all(t, in, 65536);
    GhosttyTerminalCompressionResult cr;
    OK(ghostty_terminal_compress(t, GHOSTTY_TERMINAL_COMPRESSION_MODE_FULL, &cr));
    t0 = cpu_now(); instr_start();
    // Copy-mode entry: snapshot the screen, read its text, drop it.
    for (int i = 0; i < 50; i++) {
      GhosttyTerminal c = NULL;
      OK(ghostty_terminal_clone_screen(t, &c));
      if (s->kind == K_CLONE) sink += format_len(c, GHOSTTY_FORMATTER_FORMAT_PLAIN);
      ghostty_terminal_free(c);
    }
    break;
  }
  case K_FMT_PLAIN:
  case K_FMT_VT:
    t = term(COLS, ROWS, HISTORY);
    write_all(t, in, 65536);
    t0 = cpu_now(); instr_start();
    for (int i = 0; i < 200; i++)
      sink += format_len(t, s->kind == K_FMT_PLAIN ? GHOSTTY_FORMATTER_FORMAT_PLAIN : GHOSTTY_FORMATTER_FORMAT_VT);
    break;
  case K_REFLOW:
    t = term(COLS, ROWS, HISTORY);
    write_all(t, in, 65536);
    t0 = cpu_now(); instr_start();
    for (int i = 0; i < 40; i++) {
      OK(ghostty_terminal_resize(t, 80, ROWS, 8, 16));
      OK(ghostty_terminal_resize(t, 200, ROWS, 8, 16));
      OK(ghostty_terminal_resize(t, COLS, ROWS, 8, 16));
    }
    break;
  case K_CHURN:
    t0 = cpu_now(); instr_start();
    for (int i = 0; i < 20000; i++) {
      GhosttyTerminal c = term(COLS, ROWS, HISTORY);
      ghostty_terminal_vt_write(c, (const uint8_t *)"hello\r\n", 7);
      ghostty_terminal_free(c);
    }
    break;
  case K_COMPRESS: {
    t = term(COLS, ROWS, 10 * HISTORY);
    write_all(t, in, 65536);
    t0 = cpu_now(); instr_start();
    GhosttyTerminalCompressionResult cr;
    OK(ghostty_terminal_compress(t, GHOSTTY_TERMINAL_COMPRESSION_MODE_FULL, &cr));
    break;
  }
  }
  *instr = instr_stop();
  *ms = (cpu_now() - t0) / 1e6;
  if (ci) ghostty_render_state_row_cells_free(ci);
  if (ri) ghostty_render_state_row_iterator_free(ri);
  if (rs) ghostty_render_state_free(rs);
  if (t) ghostty_terminal_free(t);
}

int main(int argc, char **argv) {
  if (argc >= 2 && strcmp(argv[1], "list") == 0) {
    for (size_t i = 0; i < NSCEN; i++) puts(scenarios[i].name);
    return 0;
  }
  if (argc < 2) {
    fprintf(stderr, "usage: %s list | <scenario> [reps]\n", argv[0]);
    return 2;
  }
  const Scenario *s = NULL;
  for (size_t i = 0; i < NSCEN; i++)
    if (strcmp(argv[1], scenarios[i].name) == 0) s = &scenarios[i];
  if (!s) {
    fprintf(stderr, "unknown scenario %s\n", argv[1]);
    return 2;
  }
  int reps = argc > 2 ? atoi(argv[2]) : 5;
  copy_reset();
  instr_open();
  Buf in = s->bytes ? make_input(s->input, s->bytes) : (Buf){0};
  double ms;
  long long instr;
  run(s, &in, &ms, &instr); // warmup
  printf("{\"scenario\":\"%s\",\"bytes\":%zu,\"cpu_ms\":[", s->name, in.len);
  long long instrs[64];
  if (reps > 64) reps = 64;
  for (int i = 0; i < reps; i++) {
    run(s, &in, &ms, &instr);
    instrs[i] = instr;
    printf("%s%.3f", i ? "," : "", ms);
  }
  printf("],\"instructions\":[");
  for (int i = 0; i < reps; i++) printf("%s%lld", i ? "," : "", instrs[i]);
  printf("],\"sink\":%llu}\n", (unsigned long long)sink);
  free(in.p);
  return 0;
}
