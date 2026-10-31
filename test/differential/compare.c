// Snapshot runner for test/differential/run.sh: built once against each engine,
// it replays the operation log from generate.py and prints hashes of the
// terminal state after every dump operation.
#include <assert.h>
#include <inttypes.h>
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
static void bytes(uint64_t *v, const void *p, size_t n) {
  const unsigned char *s = p;
  while (n--) { *v ^= *s++; *v *= UINT64_C(1099511628211); }
}
static void num(uint64_t v) { for (int i=0;i<8;i++) { unsigned char b=v>>(i*8); bytes(&h,&b,1); } }
static void blob(const void *p, size_t n) { num(n); bytes(&h,p,n); }
static void reply(GhosttyTerminal t, void *u, const uint8_t *s, size_t n) {
  (void)t; (void)u; bytes(&replies,s,n);
}
static void bell(GhosttyTerminal t, void *u) { (void)t; (void)u; bells++; }
static void title(GhosttyTerminal t, void *u) { (void)t; (void)u; titles++; }
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
static void dump_copies(GhosttyTerminal t, GhosttyRenderState clipped, uint16_t cols, uint16_t rows) {
  h = UINT64_C(14695981039346656037);
  GhosttyRenderStateClip req = { .y = rows > 2 ? 1 : 0, .rows = rows > 2 ? rows - 2 : 0, .cols = cols / 2 + 1 };
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
  OK(ghostty_render_state_clean(clipped));
  printf(" copies %016" PRIx64, h);
}
static void dump_clone(GhosttyTerminal t) {
  GhosttyTerminal c = NULL;
  OK(ghostty_terminal_clone_screen(t, &c));
  h = UINT64_C(14695981039346656037);
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
static GhosttyRenderState clipped;
static void dump(GhosttyTerminal t, GhosttyRenderState render, unsigned step) {
  h=UINT64_C(14695981039346656037);
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
  for(uint16_t y=0;y<rows;y++) for(uint16_t x=0;x<cols;x++) {
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
  OK(ghostty_render_state_update(render,t));
  GhosttyRenderStateRowIterator ri; GhosttyRenderStateRowCells ci;
  OK(ghostty_render_state_row_iterator_new(NULL,&ri)); OK(ghostty_render_state_row_cells_new(NULL,&ci));
  OK(ghostty_render_state_get(render,GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,&ri));
  while(ghostty_render_state_row_iterator_next(ri)) {
    GhosttyRow r; OK(ghostty_render_state_row_get(ri,GHOSTTY_RENDER_STATE_ROW_DATA_RAW,&r)); row(r);
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
    h=UINT64_C(14695981039346656037); blob(buf,n); printf(" fmt%d %016" PRIx64,fmt,h);
    ghostty_free(NULL,buf,n); ghostty_formatter_free(f);
  }
  dump_copies(t, clipped, cols, rows);
  dump_clone(t);
  puts(""); fflush(stdout);
}
static uint32_t read32(FILE *f) {
  unsigned char b[4]; assert(fread(b,1,4,f)==4);
  return (uint32_t)b[0]|((uint32_t)b[1]<<8)|((uint32_t)b[2]<<16)|((uint32_t)b[3]<<24);
}
int main(int argc,char **argv) {
  assert(argc==3); FILE *f=fopen(argv[1],"rb"); assert(f);
  unsigned chunk=(unsigned)strtoul(argv[2],NULL,10),step=0;
  GhosttyTerminal t=NULL; GhosttyRenderState render=NULL;
  int op;
  while((op=fgetc(f))!=EOF) {
    if(op=='N') {
      ghostty_render_state_free(render); ghostty_render_state_free(clipped); ghostty_terminal_free(t);
      uint32_t cols=read32(f),rows=read32(f); OK(ghostty_terminal_new(NULL,&t,cols,rows));
      OK(ghostty_render_state_new(NULL,&render)); OK(ghostty_render_state_new(NULL,&clipped));
      OK(ghostty_terminal_set(t,GHOSTTY_TERMINAL_OPT_WRITE_PTY,(const void*)reply));
      OK(ghostty_terminal_set(t,GHOSTTY_TERMINAL_OPT_BELL,(const void*)bell));
      OK(ghostty_terminal_set(t,GHOSTTY_TERMINAL_OPT_TITLE_CHANGED,(const void*)title));
      replies=UINT64_C(14695981039346656037); bells=titles=0;
    } else if(op=='W') {
      uint32_t n=read32(f); uint8_t *s=malloc(n?n:1); assert(s); assert(fread(s,1,n,f)==n);
      for(size_t i=0;i<n;) { size_t k=chunk?chunk:n; if(k>n-i)k=n-i; ghostty_terminal_vt_write(t,s+i,k); i+=k; }
      free(s);
    } else if(op=='R') {
      uint32_t cols=read32(f),rows=read32(f); OK(ghostty_terminal_resize(t,cols,rows,8,16));
    } else if(op=='C') {
      GhosttyTerminalCompressionResult result; OK(ghostty_terminal_compress(t,GHOSTTY_TERMINAL_COMPRESSION_MODE_FULL,&result));
    } else if(op=='K') {
      /* Continue on a clone of the active screen and drop the source. */
      GhosttyTerminal c=NULL; OK(ghostty_terminal_clone_screen(t,&c));
      ghostty_render_state_free(render); ghostty_render_state_free(clipped); ghostty_terminal_free(t);
      t=c; OK(ghostty_render_state_new(NULL,&render)); OK(ghostty_render_state_new(NULL,&clipped));
    } else if(op=='D') dump(t,render,step++);
    else abort();
  }
  ghostty_render_state_free(render); ghostty_render_state_free(clipped); ghostty_terminal_free(t); fclose(f);
  return 0;
}
