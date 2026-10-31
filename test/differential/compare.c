// Snapshot runner for test/differential/run.sh: built once against each engine,
// it replays the operation log from generate.py and prints hashes of the
// terminal state after every dump operation.
#include <assert.h>
#include <inttypes.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ghostty/vt.h>

/* Compile this exact source against each implementation's static library.
 * Hash public semantic values, never padding, pointers, or allocation IDs.
 * Input is a binary operation log emitted by generate.py. */
#define OK(expr) do { GhosttyResult r_ = (expr); if (r_ != GHOSTTY_SUCCESS) { fprintf(stderr, "line %d result %d\n", __LINE__, r_); abort(); } } while (0)
static uint64_t h, replies;
static unsigned bells, titles;
/* DIFF_TRACE=path writes every hashed value there in readable form, so two
   runs can be diffed to find the first value that differs. */
static FILE *trace;
static void mark(const char *fmt, ...) {
  if (!trace) return;
  va_list ap; va_start(ap, fmt); fputc('\n', trace); vfprintf(trace, fmt, ap); va_end(ap);
}
static void trace_str(const void *p, size_t n) {
  if (!trace) return;
  const unsigned char *s = p; fputs(" \"", trace);
  for (size_t i = 0; i < n; i++) {
    if (s[i] >= 0x20 && s[i] < 0x7f && s[i] != '"' && s[i] != '\\') fputc(s[i], trace);
    else fprintf(trace, "\\x%02x", s[i]);
  }
  fputc('"', trace);
}
static void bytes(uint64_t *v, const void *p, size_t n) {
  const unsigned char *s = p;
  while (n--) { *v ^= *s++; *v *= UINT64_C(1099511628211); }
}
static void numto(uint64_t *v, uint64_t x) {
  for (int i=0;i<8;i++) { unsigned char b=x>>(i*8); bytes(v,&b,1); }
  if (trace) fprintf(trace, " %" PRIx64, x);
}
static void num(uint64_t v) { numto(&h, v); }
static void blob(const void *p, size_t n) { num(n); bytes(&h,p,n); trace_str(p,n); }
static void reply(GhosttyTerminal t, void *u, const uint8_t *s, size_t n) {
  (void)t; (void)u; bytes(&replies,s,n); mark("reply"); trace_str(s,n);
}
static void bell(GhosttyTerminal t, void *u) { (void)t; (void)u; bells++; mark("bell"); }
static void title(GhosttyTerminal t, void *u) { (void)t; (void)u; titles++; mark("title changed"); }

/* Fuzz-only callbacks, installed by the F op. Each hashes its arguments into
   `events`, printed with every snapshot once F has run. */
static uint64_t events;
static uint32_t fuzz_flags, fuzz_unknown_max;
static bool fuzzed;
static void ev(const char *tag) { bytes(&events, tag, strlen(tag)); mark("event %s", tag); }
static void evnum(uint64_t v) { numto(&events, v); }
static void evstr(GhosttyString s) { evnum(s.len); bytes(&events, s.ptr, s.len); trace_str(s.ptr, s.len); }
static GhosttyString cstr(const char *s) { return (GhosttyString){(const uint8_t *)s, strlen(s)}; }
static void cb_clip_write(GhosttyTerminal t, void *u, const GhosttyClipboardWrite *w) {
  (void)t; (void)u; ev("clipboard write");
  evnum(w->location); evnum(w->contents_len);
  for (size_t i = 0; i < w->contents_len; i++) { evstr(w->contents[i].mime); evstr(w->contents[i].data); }
  evstr(w->name); evnum(w->granted); evnum(w->can_remember);
  GhosttyClipboardWriteReply r = GHOSTTY_INIT_SIZED(GhosttyClipboardWriteReply);
  r.result = GHOSTTY_CLIPBOARD_WRITE_RESULT_SUCCESS; r.remember = w->can_remember;
  w->reply(w, &r);
}
static void cb_clip_read(GhosttyTerminal t, void *u, const GhosttyClipboardRead *rd) {
  (void)t; (void)u; ev("clipboard read");
  evnum(rd->location); evnum(rd->mimes_len);
  for (size_t i = 0; i < rd->mimes_len; i++) evstr(rd->mimes[i]);
  evnum(rd->list); evstr(rd->name); evnum(rd->granted); evnum(rd->can_remember);
  GhosttyClipboardContent c[8]; size_t n = 0;
  for (size_t i = 0; i < rd->mimes_len && n < 8; i++) c[n++] = (GhosttyClipboardContent){rd->mimes[i], cstr("fuzz clip")};
  GhosttyString avail[2] = {cstr("text/plain"), cstr("image/png")};
  GhosttyClipboardReadReply r = GHOSTTY_INIT_SIZED(GhosttyClipboardReadReply);
  r.result = GHOSTTY_CLIPBOARD_READ_RESULT_SUCCESS; r.contents = c; r.contents_len = n;
  r.available = avail; r.available_len = 2; r.remember = rd->can_remember;
  rd->reply(rd, &r);
}
static void cb_notify(GhosttyTerminal t, void *u, const GhosttyTerminalDesktopNotification *n) {
  (void)t; (void)u; ev("notification"); evstr(n->title); evstr(n->body);
}
static void cb_progress(GhosttyTerminal t, void *u, const GhosttyTerminalProgressReport *p) {
  (void)t; (void)u; ev("progress"); evnum(p->state); evnum((uint64_t)(int64_t)p->progress);
}
static void cb_status(GhosttyTerminal t, void *u, const GhosttyTerminalProgramStatus *p) {
  (void)t; (void)u; ev("program status"); evnum(p->state); evnum(p->kind); evnum((uint64_t)(int64_t)p->progress);
  evstr(p->id); evstr(p->app); evstr(p->title); evstr(p->message);
}
static void cb_prompt(GhosttyTerminal t, void *u, const GhosttyTerminalSemanticPrompt *p) {
  (void)t; (void)u; ev("semantic prompt"); evnum(p->kind); evnum(p->prompt_kind); evnum(p->has_exit_code);
  evnum((uint64_t)(int64_t)p->exit_code); evstr(p->command); evstr(p->error);
}
static void cb_reset(GhosttyTerminal t, void *u) { (void)t; (void)u; ev("reset"); }
static void cb_pwd(GhosttyTerminal t, void *u) { (void)t; (void)u; ev("pwd"); }
static void cb_hold(GhosttyTerminal t, void *u, bool held) { (void)t; (void)u; ev("hold"); evnum(held); }
static void cb_resize(GhosttyTerminal t, void *u, const GhosttyTerminalResizeWindow *r) {
  (void)t; (void)u; ev("resize window"); evnum(r->tag); evnum(r->value.grid.rows); evnum(r->value.grid.columns);
}
static GhosttyString cb_enquiry(GhosttyTerminal t, void *u) { (void)t; (void)u; ev("enquiry"); return cstr("enq-answer"); }
static GhosttyString cb_xtversion(GhosttyTerminal t, void *u) { (void)t; (void)u; ev("xtversion"); return cstr("fuzzterm(1.0)"); }
static bool cb_size(GhosttyTerminal t, void *u, GhosttySizeReportSize *s) {
  (void)t; (void)u; ev("size"); *s = (GhosttySizeReportSize){.rows = 24, .columns = 80, .cell_width = 8, .cell_height = 16};
  return true;
}
static bool cb_scheme(GhosttyTerminal t, void *u, GhosttyColorScheme *s) {
  (void)t; (void)u; ev("color scheme"); *s = GHOSTTY_COLOR_SCHEME_DARK; return true;
}
static bool cb_da(GhosttyTerminal t, void *u, GhosttyDeviceAttributes *a) {
  (void)t; (void)u; ev("device attributes");
  memset(a, 0, sizeof *a);
  a->primary.conformance_level = 62; a->primary.features[0] = 1; a->primary.features[1] = 22; a->primary.num_features = 2;
  a->secondary.device_type = 1; a->secondary.firmware_version = 10; a->tertiary.unit_id = 0xAABBCCDD;
  return true;
}
static void cb_unknown(GhosttyTerminal t, void *u, const GhosttyTerminalUnknownSequence *s) {
  (void)t; (void)u; ev("unknown"); evnum(s->tag);
  if (s->tag == GHOSTTY_TERMINAL_UNKNOWN_SEQUENCE_APC) { evnum(s->value.apc.truncated); evstr(s->value.apc.content); }
  if (s->tag == GHOSTTY_TERMINAL_UNKNOWN_SEQUENCE_OSC) {
    evnum(s->value.osc.truncated); evstr(s->value.osc.content); evnum(s->value.osc.terminator);
  }
}
static void set_callbacks(GhosttyTerminal t) {
  OK(ghostty_terminal_set(t,GHOSTTY_TERMINAL_OPT_WRITE_PTY,(const void*)reply));
  OK(ghostty_terminal_set(t,GHOSTTY_TERMINAL_OPT_BELL,(const void*)bell));
  OK(ghostty_terminal_set(t,GHOSTTY_TERMINAL_OPT_TITLE_CHANGED,(const void*)title));
}
/* F flags: 1 event callbacks, 2 title reports, 4 checksum reports,
   8 resize keeps scrollback, 16 glyph protocol off. */
static void set_fuzz_options(GhosttyTerminal t) {
  set_callbacks(t);
  if (fuzz_flags & 1) {
    const struct { GhosttyTerminalOption opt; const void *fn; } cbs[] = {
      {GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE, (const void *)cb_clip_write},
      {GHOSTTY_TERMINAL_OPT_CLIPBOARD_READ, (const void *)cb_clip_read},
      {GHOSTTY_TERMINAL_OPT_DESKTOP_NOTIFICATION, (const void *)cb_notify},
      {GHOSTTY_TERMINAL_OPT_PROGRESS_REPORT, (const void *)cb_progress},
      {GHOSTTY_TERMINAL_OPT_PROGRAM_STATUS, (const void *)cb_status},
      {GHOSTTY_TERMINAL_OPT_SEMANTIC_PROMPT, (const void *)cb_prompt},
      {GHOSTTY_TERMINAL_OPT_RESET, (const void *)cb_reset},
      {GHOSTTY_TERMINAL_OPT_PWD_CHANGED, (const void *)cb_pwd},
      {GHOSTTY_TERMINAL_OPT_RENDER_HOLD, (const void *)cb_hold},
      {GHOSTTY_TERMINAL_OPT_RESIZE_WINDOW, (const void *)cb_resize},
      {GHOSTTY_TERMINAL_OPT_ENQUIRY, (const void *)cb_enquiry},
      {GHOSTTY_TERMINAL_OPT_XTVERSION, (const void *)cb_xtversion},
      {GHOSTTY_TERMINAL_OPT_SIZE, (const void *)cb_size},
      {GHOSTTY_TERMINAL_OPT_COLOR_SCHEME, (const void *)cb_scheme},
      {GHOSTTY_TERMINAL_OPT_DEVICE_ATTRIBUTES, (const void *)cb_da},
      {GHOSTTY_TERMINAL_OPT_UNKNOWN_SEQUENCE, (const void *)cb_unknown},
    };
    for (size_t i = 0; i < sizeof cbs / sizeof *cbs; i++) OK(ghostty_terminal_set(t, cbs[i].opt, cbs[i].fn));
  }
  size_t unknown = fuzz_unknown_max; OK(ghostty_terminal_set(t, GHOSTTY_TERMINAL_OPT_UNKNOWN_MAX_BYTES, &unknown));
  bool on = true, off = false;
  if (fuzz_flags & 2) OK(ghostty_terminal_set(t, GHOSTTY_TERMINAL_OPT_TITLE_REPORT, &on));
  if (fuzz_flags & 4) OK(ghostty_terminal_set(t, GHOSTTY_TERMINAL_OPT_XT_CHECKSUM_REPORT, &on));
  if (fuzz_flags & 8) OK(ghostty_terminal_set(t, GHOSTTY_TERMINAL_OPT_RESIZE_PULL_SCROLLBACK, &off));
  if (fuzz_flags & 16) OK(ghostty_terminal_set(t, GHOSTTY_TERMINAL_OPT_GLYPH_PROTOCOL, &off));
}
/* Tracked grid refs from T ops, checked by every fuzz snapshot. */
static GhosttyTrackedGridRef tracked[8];
static size_t ntracked;
static void free_tracked(void) {
  for (size_t i = 0; i < ntracked; i++) ghostty_tracked_grid_ref_free(tracked[i]);
  ntracked = 0;
}
static void point_of(GhosttyTerminal t, const GhosttyGridRef *ref) {
  GhosttyPointCoordinate pt = {0};
  GhosttyResult r = ghostty_terminal_point_from_grid_ref(t, ref, GHOSTTY_POINT_TAG_SCREEN, &pt);
  num(r); if (r == GHOSTTY_SUCCESS) { num(pt.x); num(pt.y); }
}
static void format_selection(GhosttyTerminal t) {
  mark("selection");
  GhosttySelection sel = GHOSTTY_INIT_SIZED(GhosttySelection);
  GhosttyResult r = ghostty_terminal_get(t, GHOSTTY_TERMINAL_DATA_SELECTION, &sel);
  num(r);
  if (r != GHOSTTY_SUCCESS) return;
  point_of(t, &sel.start); point_of(t, &sel.end); num(sel.rectangle);
  for (int fmt = 0; fmt < 3; fmt++) for (int k = 0; k < 2; k++) {
    GhosttyTerminalSelectionFormatOptions o = GHOSTTY_INIT_SIZED(GhosttyTerminalSelectionFormatOptions);
    o.emit = fmt; o.unwrap = o.trim = k;
    uint8_t *buf = NULL; size_t n = 0;
    r = ghostty_terminal_selection_format_alloc(t, NULL, o, &buf, &n);
    num(r); if (r == GHOSTTY_SUCCESS) blob(buf, n);
    ghostty_free(NULL, buf, n);
  }
}
/* Terminal values the snapshot does not cover otherwise. */
static void dump_extra(GhosttyTerminal t, GhosttyRenderState render) {
  h = UINT64_C(14695981039346656037);
  mark("extra");
  GhosttyString pwd = {0}; OK(ghostty_terminal_get(t, GHOSTTY_TERMINAL_DATA_PWD, &pwd)); blob(pwd.ptr, pwd.len);
  for (int k = GHOSTTY_TERMINAL_DATA_COLOR_FOREGROUND; k <= GHOSTTY_TERMINAL_DATA_COLOR_CURSOR_DEFAULT; k++) {
    if (k == GHOSTTY_TERMINAL_DATA_COLOR_PALETTE) continue;
    GhosttyColorRgb c = {0}; GhosttyResult r = ghostty_terminal_get(t, k, &c);
    num(r); if (r == GHOSTTY_SUCCESS) { num(c.r); num(c.g); num(c.b); }
  }
  GhosttyColorRgb pal[256]; OK(ghostty_terminal_get(t, GHOSTTY_TERMINAL_DATA_COLOR_PALETTE, pal));
  for (int i = 0; i < 256; i++) { num(pal[i].r); num(pal[i].g); num(pal[i].b); }
  size_t z = 0;
  const int sizes[] = {GHOSTTY_TERMINAL_DATA_TOTAL_ROWS, GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS};
  for (size_t i = 0; i < sizeof sizes / sizeof *sizes; i++) { z = 0; OK(ghostty_terminal_get(t, sizes[i], &z)); num(z); }
  const int bools[] = {GHOSTTY_TERMINAL_DATA_VIEWPORT_ACTIVE, GHOSTTY_TERMINAL_DATA_VT_PROCESSING_ERROR,
    GHOSTTY_TERMINAL_DATA_VT_GROUND, GHOSTTY_TERMINAL_DATA_CURSOR_AT_PROMPT};
  for (size_t i = 0; i < sizeof bools / sizeof *bools; i++) { bool b = false; OK(ghostty_terminal_get(t, bools[i], &b)); num(b); }
  GhosttyMouseShape shape = 0; OK(ghostty_terminal_get(t, GHOSTTY_TERMINAL_DATA_MOUSE_SHAPE, &shape)); num(shape);
  GhosttyRenderStateClip clip = {0}; OK(ghostty_render_state_get(render, GHOSTTY_RENDER_STATE_DATA_CLIP, &clip));
  num(clip.y); num(clip.rows); num(clip.cols);
  GhosttyRenderStateOverscan over = {0}; OK(ghostty_render_state_get(render, GHOSTTY_RENDER_STATE_DATA_OVERSCAN, &over));
  num(over.above); num(over.below);
  GhosttyRenderStateCursor cur = GHOSTTY_INIT_SIZED(GhosttyRenderStateCursor);
  OK(ghostty_render_state_get(render, GHOSTTY_RENDER_STATE_DATA_CURSOR, &cur));
  num(cur.viewport_has_value);
  if (cur.viewport_has_value) { num(cur.viewport_x); num(cur.viewport_y); num(cur.wide_tail); }
  num(cur.visible); num(cur.blinking); num(cur.password_input); num(cur.visual_style);
  GhosttyRenderStateColors col = GHOSTTY_INIT_SIZED(GhosttyRenderStateColors);
  OK(ghostty_render_state_get(render, GHOSTTY_RENDER_STATE_DATA_COLORS, &col));
  num(col.background.r); num(col.background.g); num(col.background.b);
  num(col.foreground.r); num(col.foreground.g); num(col.foreground.b);
  num(col.cursor_has_value); if (col.cursor_has_value) { num(col.cursor.r); num(col.cursor.g); num(col.cursor.b); }
  for (int i = 0; i < 256; i++) { num(col.palette[i].r); num(col.palette[i].g); num(col.palette[i].b); }
  /* Row positions and selection ranges of the main render state's rows. */
  mark("render rows");
  GhosttyRenderStateRowIterator ri; OK(ghostty_render_state_row_iterator_new(NULL, &ri));
  OK(ghostty_render_state_get(render, GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, &ri));
  while (ghostty_render_state_row_iterator_next(ri)) {
    int32_t vy = 0; OK(ghostty_render_state_row_get(ri, GHOSTTY_RENDER_STATE_ROW_DATA_VIEWPORT_Y, &vy)); num((uint32_t)vy);
    GhosttyRenderStateRowSelection rs = GHOSTTY_INIT_SIZED(GhosttyRenderStateRowSelection);
    GhosttyResult r = ghostty_render_state_row_get(ri, GHOSTTY_RENDER_STATE_ROW_DATA_SELECTION, &rs);
    num(r); if (r == GHOSTTY_SUCCESS) { num(rs.start_x); num(rs.end_x); }
  }
  ghostty_render_state_row_iterator_free(ri);
  format_selection(t);
  mark("tracked");
  for (size_t i = 0; i < ntracked; i++) {
    bool has = ghostty_tracked_grid_ref_has_value(tracked[i]); num(has);
    for (int tag = GHOSTTY_POINT_TAG_ACTIVE; tag <= GHOSTTY_POINT_TAG_HISTORY; tag++) {
      GhosttyPointCoordinate pt = {0};
      GhosttyResult r = ghostty_tracked_grid_ref_point(tracked[i], (GhosttyPointTag)tag, &pt);
      num(r); if (r == GHOSTTY_SUCCESS) { num(pt.x); num(pt.y); }
    }
  }
  mark("unwrapped");
  for (int fmt = 0; fmt < 2; fmt++) {
    GhosttyFormatterTerminalOptions o = GHOSTTY_INIT_SIZED(GhosttyFormatterTerminalOptions);
    o.emit = fmt; o.unwrap = true; o.trim = true;
    o.extra = (GhosttyFormatterTerminalExtra)GHOSTTY_INIT_SIZED(GhosttyFormatterTerminalExtra);
    o.extra.screen = (GhosttyFormatterScreenExtra)GHOSTTY_INIT_SIZED(GhosttyFormatterScreenExtra);
    GhosttyFormatter f; OK(ghostty_formatter_terminal_new(NULL, &f, t, o));
    uint8_t *buf = NULL; size_t n = 0; OK(ghostty_formatter_format_alloc(f, NULL, &buf, &n));
    blob(buf, n); ghostty_free(NULL, buf, n); ghostty_formatter_free(f);
  }
  if (trace) {
    /* Page structure, for triage only: pool reuse may differ by design. */
    GhosttyTerminalMemoryUsage m = GHOSTTY_INIT_SIZED(GhosttyTerminalMemoryUsage);
    if (ghostty_terminal_get(t, GHOSTTY_TERMINAL_DATA_MEMORY_USAGE, &m) == GHOSTTY_SUCCESS)
      mark("memory pages %" PRIu64 " virtual %" PRIu64 " compressed %" PRIu64 " alt pages %" PRIu64,
           m.primary_pages, m.primary_virtual_bytes, m.primary_compressed_pages, m.alternate_pages);
  }
  printf(" events %016" PRIx64 " extra %016" PRIx64, events, h);
}
static void color(GhosttyStyleColor c) {
  num(c.tag);
  if(c.tag==GHOSTTY_STYLE_COLOR_PALETTE) num(c.value.palette);
  if(c.tag==GHOSTTY_STYLE_COLOR_RGB) {
    num(c.value.rgb.r); num(c.value.rgb.g); num(c.value.rgb.b);
  }
}
static void style(GhosttyStyle s) {
  color(s.fg_color); color(s.bg_color); color(s.underline_color);
  num(s.bold); num(s.italic); num(s.faint); num(s.blink); num(s.inverse);
  num(s.invisible); num(s.strikethrough); num(s.overline); num(s.underline);
}
static void cell(GhosttyCell c) {
  mark("cell");
  const GhosttyCellData ints[]={GHOSTTY_CELL_DATA_CODEPOINT,
    GHOSTTY_CELL_DATA_CONTENT_TAG,GHOSTTY_CELL_DATA_WIDE,GHOSTTY_CELL_DATA_SEMANTIC_CONTENT};
  for(size_t i=0;i<sizeof(ints)/sizeof(*ints);i++) {
    uint32_t v=0; OK(ghostty_cell_get(c,ints[i],&v)); num(v);
  }
  const GhosttyCellData bools[]={GHOSTTY_CELL_DATA_HAS_TEXT,GHOSTTY_CELL_DATA_HAS_STYLING,
    GHOSTTY_CELL_DATA_HAS_HYPERLINK,GHOSTTY_CELL_DATA_PROTECTED};
  for(size_t i=0;i<sizeof(bools)/sizeof(*bools);i++) {
    bool v=false; OK(ghostty_cell_get(c,bools[i],&v)); num(v);
  }
  uint8_t pal=0; GhosttyResult r=ghostty_cell_get(c,GHOSTTY_CELL_DATA_COLOR_PALETTE,&pal);
  num(r); if(r==GHOSTTY_SUCCESS) num(pal);
  GhosttyColorRgb rgb={0}; r=ghostty_cell_get(c,GHOSTTY_CELL_DATA_COLOR_RGB,&rgb);
  num(r); if(r==GHOSTTY_SUCCESS) { num(rgb.r); num(rgb.g); num(rgb.b); }
}
static void row(GhosttyRow r) {
  for(int k=GHOSTTY_ROW_DATA_WRAP;k<=GHOSTTY_ROW_DATA_HYPERLINK;k++) {
    bool v=false; OK(ghostty_row_get(r,(GhosttyRowData)k,&v)); num(v);
  }
}

/* zz additions: packed row copies (with clip and trim) and screen clones. */
static GhosttyRenderStateCell *cp_cells; static GhosttyStyle *cp_styles;
static GhosttyRenderStateGrapheme *cp_graph; static uint8_t *cp_bytes;
static size_t cp_cells_cap, cp_styles_cap, cp_graph_cap, cp_bytes_cap;
static void grow(void **p, size_t *cap, size_t need, size_t elem) {
  if (need <= *cap) return; *cap = need * 2; *p = realloc(*p, *cap * elem); assert(*p);
}
static void copy_row(GhosttyRenderStateRowCells ci, uint16_t cols, uint32_t flags) {
  for (;;) {
    GhosttyRenderStateRowCellsCopy o = GHOSTTY_INIT_SIZED(GhosttyRenderStateRowCellsCopy);
    o.cells = cp_cells; o.cells_cap = cp_cells_cap;
    o.styles = cp_styles; o.styles_cap = cp_styles_cap;
    o.graphemes = cp_graph; o.graphemes_cap = cp_graph_cap;
    o.grapheme_bytes = cp_bytes; o.grapheme_bytes_cap = cp_bytes_cap;
    o.flags = flags;
    GhosttyResult r = ghostty_render_state_row_cells_copy(ci, 0, cols, &o);
    if (r == GHOSTTY_OUT_OF_SPACE) {
      grow((void **)&cp_cells, &cp_cells_cap, o.cells_len, sizeof *cp_cells);
      grow((void **)&cp_styles, &cp_styles_cap, o.styles_len, sizeof *cp_styles);
      grow((void **)&cp_graph, &cp_graph_cap, o.graphemes_len, sizeof *cp_graph);
      grow((void **)&cp_bytes, &cp_bytes_cap, o.grapheme_bytes_len, 1);
      continue;
    }
    OK(r);
    mark("copy row");
    num(o.cells_len);
    for (size_t i = 0; i < o.cells_len; i++) {
      GhosttyRenderStateCell c = cp_cells[i];
      num(c.content); num(c.content_tag); num(c.wide); num(c.flags); num(c.semantic_content);
      /* Style and grapheme table indexes depend on the dedup cache, so hash
         what they point at. */
      assert(c.style < o.styles_len); style(cp_styles[c.style]);
      num(c.grapheme != 0);
      if (c.grapheme) {
        assert(c.grapheme < o.graphemes_len + 1);
        GhosttyRenderStateGrapheme g = cp_graph[c.grapheme - 1];
        blob(cp_bytes + g.offset, g.len);
      }
    }
    return;
  }
}
/* Each dump copies through a fresh clipped render state. A second render state
   kept across dumps would see row dirty flags the main one already consumed,
   so whether it rebuilt a row would depend on page addresses being reused. */
static void dump_copies(GhosttyTerminal t, uint16_t cols, uint16_t rows) {
  h = UINT64_C(14695981039346656037);
  GhosttyRenderState clipped; OK(ghostty_render_state_new(NULL, &clipped));
  GhosttyRenderStateClip req = { .y = rows > 2 ? 1 : 0, .rows = rows > 2 ? rows - 2 : 0, .cols = cols / 2 + 1 };
  mark("copies");
  OK(ghostty_render_state_set(clipped, GHOSTTY_RENDER_STATE_OPTION_CLIP, &req));
  OK(ghostty_render_state_update(clipped, t));
  GhosttyRenderStateClip got = {0};
  OK(ghostty_render_state_get(clipped, GHOSTTY_RENDER_STATE_DATA_CLIP, &got));
  num(got.y); num(got.rows); num(got.cols);
  GhosttyRenderStateRowIterator ri; GhosttyRenderStateRowCells ci;
  OK(ghostty_render_state_row_iterator_new(NULL, &ri)); OK(ghostty_render_state_row_cells_new(NULL, &ci));
  OK(ghostty_render_state_get(clipped, GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, &ri));
  while (ghostty_render_state_row_iterator_next(ri)) {
    OK(ghostty_render_state_row_get(ri, GHOSTTY_RENDER_STATE_ROW_DATA_CELLS, &ci));
    copy_row(ci, cols, 0);
    copy_row(ci, cols, GHOSTTY_RENDER_STATE_ROW_CELLS_COPY_TRIM);
  }
  ghostty_render_state_row_cells_free(ci); ghostty_render_state_row_iterator_free(ri);
  ghostty_render_state_free(clipped);
  printf(" copies %016" PRIx64, h);
}
static void dump_clone(GhosttyTerminal t) {
  GhosttyTerminal c = NULL;
  OK(ghostty_terminal_clone_screen(t, &c));
  h = UINT64_C(14695981039346656037);
  mark("clone");
  uint16_t v = 0;
  for (int k = 1; k <= 4; k++) { v = 0; OK(ghostty_terminal_get(c, k, &v)); num(v); }
  GhosttyTerminalScrollbar bar = {0}; OK(ghostty_terminal_get(c, GHOSTTY_TERMINAL_DATA_SCROLLBAR, &bar));
  num(bar.total); num(bar.offset); num(bar.len);
  for (int fmt = 0; fmt < 2; fmt++) {
    GhosttyFormatterTerminalOptions o = GHOSTTY_INIT_SIZED(GhosttyFormatterTerminalOptions);
    o.emit = fmt;
    o.extra = (GhosttyFormatterTerminalExtra)GHOSTTY_INIT_SIZED(GhosttyFormatterTerminalExtra);
    o.extra.screen = (GhosttyFormatterScreenExtra)GHOSTTY_INIT_SIZED(GhosttyFormatterScreenExtra);
    o.extra.screen.cursor = o.extra.screen.style = o.extra.screen.hyperlink = true;
    GhosttyFormatter f; OK(ghostty_formatter_terminal_new(NULL, &f, c, o));
    uint8_t *buf = NULL; size_t n = 0; OK(ghostty_formatter_format_alloc(f, NULL, &buf, &n));
    blob(buf, n); ghostty_free(NULL, buf, n); ghostty_formatter_free(f);
  }
  ghostty_terminal_free(c);
  printf(" clone %016" PRIx64, h);
}
static void dump(GhosttyTerminal t, GhosttyRenderState render, unsigned step) {
  h=UINT64_C(14695981039346656037);
  mark("snapshot %u state", step);
  uint16_t cols=0,rows=0;
  OK(ghostty_terminal_get(t,GHOSTTY_TERMINAL_DATA_COLS,&cols));
  OK(ghostty_terminal_get(t,GHOSTTY_TERMINAL_DATA_ROWS,&rows));
  for(int k=1;k<=4;k++) { uint16_t v=0; OK(ghostty_terminal_get(t,k,&v)); num(v); }
  const int bk[]={GHOSTTY_TERMINAL_DATA_CURSOR_PENDING_WRAP,
    GHOSTTY_TERMINAL_DATA_CURSOR_VISIBLE,GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING};
  for(size_t i=0;i<sizeof(bk)/sizeof(*bk);i++) {
    bool v=false; OK(ghostty_terminal_get(t,bk[i],&v)); num(v);
  }
  GhosttyTerminalScreen active=0; OK(ghostty_terminal_get(t,GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN,&active)); num(active);
  uint8_t flags=0; OK(ghostty_terminal_get(t,GHOSTTY_TERMINAL_DATA_KITTY_KEYBOARD_FLAGS,&flags)); num(flags);
  GhosttyTerminalScrollbar bar={0}; OK(ghostty_terminal_get(t,GHOSTTY_TERMINAL_DATA_SCROLLBAR,&bar));
  num(bar.total); num(bar.offset); num(bar.len);
  GhosttyString title_={0}; OK(ghostty_terminal_get(t,GHOSTTY_TERMINAL_DATA_TITLE,&title_)); blob(title_.ptr,title_.len);
  GhosttyStyle s=GHOSTTY_INIT_SIZED(GhosttyStyle);
  OK(ghostty_terminal_get(t,GHOSTTY_TERMINAL_DATA_CURSOR_STYLE,&s)); style(s);
  const int modes[]={1,3,5,6,7,12,25,45,47,69,1000,1002,1003,1004,1006,1047,1049,2004,2026,2048};
  for(size_t i=0;i<sizeof(modes)/sizeof(*modes);i++) {
    GhosttyTerminalModeConfig m={.mode=ghostty_mode_new(modes[i],false)};
    num(ghostty_terminal_get(t,GHOSTTY_TERMINAL_DATA_MODE,&m)); num(m.value);
  }
  printf("%u state %016" PRIx64 " callbacks %016" PRIx64 "/%u/%u",step,h,replies,bells,titles);
  h=UINT64_C(14695981039346656037);
  mark("grid");
  for(uint16_t y=0;y<rows;y++) for(uint16_t x=0;x<cols;x++) {
    mark("grid %u,%u", y, x);
    GhosttyGridRef ref=GHOSTTY_INIT_SIZED(GhosttyGridRef);
    GhosttyPoint p={.tag=GHOSTTY_POINT_TAG_ACTIVE,.value={.coordinate={.x=x,.y=y}}};
    OK(ghostty_terminal_grid_ref(t,p,&ref));
    GhosttyCell c; OK(ghostty_grid_ref_cell(&ref,&c)); cell(c);
    if(!x) { GhosttyRow r; OK(ghostty_grid_ref_row(&ref,&r)); row(r); }
    uint32_t cps[1024]; size_t n=0;
    OK(ghostty_grid_ref_graphemes(&ref,cps,1024,&n)); num(n);
    for(size_t j=0;j<n;j++) num(cps[j]);
    s=(GhosttyStyle)GHOSTTY_INIT_SIZED(GhosttyStyle); OK(ghostty_grid_ref_style(&ref,&s)); style(s);
  }
  printf(" grid %016" PRIx64,h);
  h=UINT64_C(14695981039346656037);
  mark("render");
  OK(ghostty_render_state_update(render,t));
  GhosttyRenderStateRowIterator ri; GhosttyRenderStateRowCells ci;
  OK(ghostty_render_state_row_iterator_new(NULL,&ri)); OK(ghostty_render_state_row_cells_new(NULL,&ci));
  OK(ghostty_render_state_get(render,GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,&ri));
  while(ghostty_render_state_row_iterator_next(ri)) {
    mark("render row"); GhosttyRow r; OK(ghostty_render_state_row_get(ri,GHOSTTY_RENDER_STATE_ROW_DATA_RAW,&r)); row(r);
    OK(ghostty_render_state_row_get(ri,GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,&ci));
    while(ghostty_render_state_row_cells_next(ci)) {
      GhosttyCell c; OK(ghostty_render_state_row_cells_get(ci,GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW,&c)); cell(c);
      s=(GhosttyStyle)GHOSTTY_INIT_SIZED(GhosttyStyle);
      OK(ghostty_render_state_row_cells_get(ci,GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE,&s)); style(s);
      uint32_t cps[1024],n=0; OK(ghostty_render_state_row_cells_get(ci,GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_LEN,&n));
      assert(n<=1024); OK(ghostty_render_state_row_cells_get(ci,GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF,cps));
      num(n); for(uint32_t j=0;j<n;j++)num(cps[j]);
    }
  }
  ghostty_render_state_row_cells_free(ci); ghostty_render_state_row_iterator_free(ri);
  OK(ghostty_render_state_clean(render));
  printf(" render %016" PRIx64,h);
  for(int fmt=0;fmt<3;fmt++) {
    GhosttyFormatterTerminalOptions o=GHOSTTY_INIT_SIZED(GhosttyFormatterTerminalOptions);
    o.emit=fmt; o.unwrap=false; o.trim=false;
    o.extra=(GhosttyFormatterTerminalExtra)GHOSTTY_INIT_SIZED(GhosttyFormatterTerminalExtra);
    o.extra.palette=o.extra.modes=o.extra.scrolling_region=o.extra.tabstops=o.extra.pwd=o.extra.keyboard=true;
    o.extra.screen=(GhosttyFormatterScreenExtra)GHOSTTY_INIT_SIZED(GhosttyFormatterScreenExtra);
    o.extra.screen.cursor=o.extra.screen.style=o.extra.screen.hyperlink=o.extra.screen.protection=o.extra.screen.kitty_keyboard=o.extra.screen.charsets=true;
    GhosttyFormatter f; OK(ghostty_formatter_terminal_new(NULL,&f,t,o));
    uint8_t *buf=NULL; size_t n=0; OK(ghostty_formatter_format_alloc(f,NULL,&buf,&n));
    h=UINT64_C(14695981039346656037); mark("fmt%d", fmt); blob(buf,n); printf(" fmt%d %016" PRIx64,fmt,h);
    ghostty_free(NULL,buf,n); ghostty_formatter_free(f);
  }
  dump_copies(t, cols, rows);
  dump_clone(t);
  if (fuzzed) dump_extra(t, render);
  puts(""); fflush(stdout);
}
static uint32_t read32(FILE *f) {
  unsigned char b[4]; assert(fread(b,1,4,f)==4);
  return (uint32_t)b[0]|((uint32_t)b[1]<<8)|((uint32_t)b[2]<<16)|((uint32_t)b[3]<<24);
}
int main(int argc,char **argv) {
  assert(argc==3); FILE *f=fopen(argv[1],"rb"); assert(f);
  const char *tp=getenv("DIFF_TRACE"); if(tp) { trace=fopen(tp,"w"); assert(trace); }
  unsigned chunk=(unsigned)strtoul(argv[2],NULL,10),step=0;
  GhosttyTerminal t=NULL; GhosttyRenderState render=NULL;
  int op;
  while((op=fgetc(f))!=EOF) {
    if(op=='N') {
      ghostty_render_state_free(render); ghostty_terminal_free(t); free_tracked();
      uint32_t cols=read32(f),rows=read32(f); OK(ghostty_terminal_new(NULL,&t,cols,rows));
      OK(ghostty_render_state_new(NULL,&render));
      set_callbacks(t); mark("op new %u %u", cols, rows);
      replies=events=UINT64_C(14695981039346656037); bells=titles=0; fuzzed=false;
    } else if(op=='W') {
      uint32_t n=read32(f); uint8_t *s=malloc(n?n:1); assert(s); assert(fread(s,1,n,f)==n);
      mark("op write %u", n); trace_str(s, n);
      for(size_t i=0;i<n;) { size_t k=chunk?chunk:n; if(k>n-i)k=n-i; ghostty_terminal_vt_write(t,s+i,k); i+=k; }
      free(s);
    } else if(op=='R') {
      uint32_t cols=read32(f),rows=read32(f); mark("op resize %u %u", cols, rows);
      GhosttyResult r=ghostty_terminal_resize(t,cols,rows,8,16);
      if(r!=GHOSTTY_SUCCESS) { printf("resize %u %u result %d\n",cols,rows,r); fflush(stdout); }
    } else if(op=='C') {
      mark("op compress");
      GhosttyTerminalCompressionResult result; OK(ghostty_terminal_compress(t,GHOSTTY_TERMINAL_COMPRESSION_MODE_FULL,&result));
    } else if(op=='K') {
      /* Continue on a clone of the active screen and drop the source. */
      mark("op clone");
      GhosttyTerminal c=NULL; OK(ghostty_terminal_clone_screen(t,&c));
      ghostty_render_state_free(render); ghostty_terminal_free(t);
      t=c; OK(ghostty_render_state_new(NULL,&render));
      if(fuzzed) set_fuzz_options(t);
    } else if(op=='D') dump(t,render,step++);
    /* The ops below only appear in fuzz cases (generate.py --seed). */
    else if(op=='F') {
      fuzz_flags=read32(f); fuzz_unknown_max=read32(f); fuzzed=true;
      mark("op fuzz options %u %u", fuzz_flags, fuzz_unknown_max); set_fuzz_options(t);
    } else if(op=='V') {
      uint32_t tag=read32(f),v=read32(f); mark("op viewport %u %d", tag, (int32_t)v);
      GhosttyTerminalScrollViewport sv={.tag=(GhosttyTerminalScrollViewportTag)tag};
      if(tag==GHOSTTY_SCROLL_VIEWPORT_ROW) sv.value.row=v; else sv.value.delta=(int32_t)v;
      ghostty_terminal_scroll_viewport(t,sv);
    } else if(op=='X') { mark("op reset"); ghostty_terminal_reset(t); }
    else if(op=='S') {
      /* Install a selection: 0 word, 1 line, 2 all, 3 command output at the
         active point (x, y), anything else clears it. */
      uint32_t kind=read32(f),x=read32(f),y=read32(f); mark("op select %u %u %u", kind, x, y);
      GhosttySelection sel=GHOSTTY_INIT_SIZED(GhosttySelection);
      GhosttyGridRef ref=GHOSTTY_INIT_SIZED(GhosttyGridRef);
      GhosttyPoint p={.tag=GHOSTTY_POINT_TAG_ACTIVE,.value={.coordinate={.x=x,.y=y}}};
      GhosttyResult r=ghostty_terminal_grid_ref(t,p,&ref);
      if(kind>3) r=ghostty_terminal_set(t,GHOSTTY_TERMINAL_OPT_SELECTION,NULL);
      else if(r==GHOSTTY_SUCCESS) {
        if(kind==0) {
          GhosttyTerminalSelectWordOptions o=GHOSTTY_INIT_SIZED(GhosttyTerminalSelectWordOptions); o.ref=ref;
          r=ghostty_terminal_select_word(t,&o,&sel);
        } else if(kind==1) {
          GhosttyTerminalSelectLineOptions o=GHOSTTY_INIT_SIZED(GhosttyTerminalSelectLineOptions); o.ref=ref;
          o.semantic_prompt_boundary=x&1;
          r=ghostty_terminal_select_line(t,&o,&sel);
        } else if(kind==2) r=ghostty_terminal_select_all(t,&sel);
        else r=ghostty_terminal_select_output(t,ref,&sel);
        if(r==GHOSTTY_SUCCESS) r=ghostty_terminal_set(t,GHOSTTY_TERMINAL_OPT_SELECTION,&sel);
      }
      evnum(r); mark("result %d", r);
    } else if(op=='T') {
      /* Track the active point (x, y) through everything that follows. */
      uint32_t x=read32(f),y=read32(f); mark("op track %u %u", x, y);
      GhosttyPoint p={.tag=GHOSTTY_POINT_TAG_ACTIVE,.value={.coordinate={.x=x,.y=y}}};
      GhosttyTrackedGridRef ref=NULL; GhosttyResult r=ghostty_terminal_grid_ref_track(t,p,&ref);
      evnum(r);
      if(r==GHOSTTY_SUCCESS) { if(ntracked<8) tracked[ntracked++]=ref; else ghostty_tracked_grid_ref_free(ref); }
    }
    else if(op=='P') {
      /* Clip and overscan the main render state, so its incremental
         updates run through those paths. */
      GhosttyRenderStateClip c={0}; GhosttyRenderStateOverscan o={0};
      c.y=read32(f); c.rows=read32(f); c.cols=read32(f); o.above=read32(f); o.below=read32(f);
      mark("op render clip %u %u %u overscan %u %u", c.y, c.rows, c.cols, o.above, o.below);
      OK(ghostty_render_state_set(render,GHOSTTY_RENDER_STATE_OPTION_CLIP,&c));
      OK(ghostty_render_state_set(render,GHOSTTY_RENDER_STATE_OPTION_OVERSCAN,&o));
    }
    else if(op=='L') {
      uint32_t kind=read32(f),v=read32(f); size_t limit=v; mark("op scrollback limit %u %u", kind, v);
      GhosttyResult r=ghostty_terminal_set(t,kind?GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES:GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES,&limit);
      if(r!=GHOSTTY_SUCCESS) { printf("scrollback limit %u %u result %d\n",kind,v,r); fflush(stdout); }
    }
    else abort();
  }
  ghostty_render_state_free(render); ghostty_terminal_free(t); free_tracked(); fclose(f);
  if (trace) fclose(trace);
  return 0;
}
