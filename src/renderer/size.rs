//! Port of `src/renderer/size.zig`.
//!
//! STATUS: ported

use crate::terminal::size as terminal_size;

/// Controls how extra whitespace around the terminal grid is distributed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PaddingBalance {
    /// No balancing; padding is applied as specified explicitly.
    #[default]
    False,
    /// Balances padding but caps the top padding so the first row doesn't
    /// drift too far from the top of the window. Excess vertical space is
    /// shifted to the bottom.
    True,
    /// Distributes leftover space equally on all sides so the grid is
    /// centered within the screen.
    Equal,
}

impl PaddingBalance {
    /// The Zig tag name (used as the config value).
    pub fn as_str(self) -> &'static str {
        match self {
            PaddingBalance::False => "false",
            PaddingBalance::True => "true",
            PaddingBalance::Equal => "equal",
        }
    }
}

impl std::str::FromStr for PaddingBalance {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "false" => Ok(PaddingBalance::False),
            "true" => Ok(PaddingBalance::True),
            "equal" => Ok(PaddingBalance::Equal),
            _ => Err(()),
        }
    }
}

/// All relevant sizes for a rendered terminal. These are all the sizes that
/// any functionality should need to know about the terminal in order to
/// convert between any coordinate systems.
///
/// See the individual field type documentation for more information on each
/// field. One important note is that any pixel values should already be scaled
/// to the current DPI of the screen. If the DPI changes, the sizes should be
/// recalculated and we expect this to be done by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Size {
    pub screen: ScreenSize,
    pub cell: CellSize,
    pub padding: Padding,
}

impl Size {
    /// Return the grid size for this size. The grid size is calculated by
    /// taking the screen size, removing padding, and dividing by the cell
    /// dimensions.
    pub fn grid(self) -> GridSize {
        GridSize::init(self.screen.sub_padding(self.padding), self.cell)
    }

    /// The size of the terminal. This is the same as the screen without
    /// padding.
    pub fn terminal(self) -> ScreenSize {
        self.screen.sub_padding(self.padding)
    }

    /// Set the padding to be balanced around the grid. The balanced
    /// padding is calculated AFTER the explicit padding is taken
    /// into account.
    pub fn balance_padding(&mut self, explicit: Padding, mode: PaddingBalance) {
        // This ensure grid() does the right thing
        self.padding = explicit;

        // Now we can calculate the balanced padding
        self.padding = Padding::balanced(self.screen, self.grid(), self.cell);

        match mode {
            PaddingBalance::False => unreachable!(),
            PaddingBalance::Equal => {}
            PaddingBalance::True => {
                // Cap the top padding to avoid excessive space above the
                // first row. The maximum is the balanced explicit horizontal
                // padding plus half a cell width. Any excess is shifted to
                // the bottom.
                let max_top = (explicit.left + explicit.right + self.cell.width) / 2;
                let vshift = self.padding.top.saturating_sub(max_top);
                self.padding.top -= vshift;
                self.padding.bottom += vshift;
            }
        }
    }
}

/// A coordinate. This is defined as a tagged union to allow for different
/// coordinate systems to be represented.
///
/// A coordinate is only valid within the context of a stable Size value.
/// If any of the sizes in the Size struct change, the coordinate is no
/// longer valid and must be recalculated. A conversion function is provided
/// to migrate to a new Size (which may result in failure).
///
/// The coordinate systems are:
///
///   * surface: (0, 0) is the top-left of the surface (with padding). Negative
///     values are allowed and are off the surface. Likewise, values greater
///     than the surface size are off the surface. Units are pixels.
///
///   * terminal: (0, 0) is the top-left of the terminal grid. This is the
///     same as the surface but with the padding removed. Negative values and
///     values greater than the grid size are allowed and are off the terminal.
///     Units are pixels.
///
///   * grid: (0, 0) is the top-left of the grid. Units are in cells. Negative
///     values are not allowed but values greater than the grid size are
///     possible and are off the grid.
///
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Coordinate {
    Surface(CoordinateSurface),
    Terminal(CoordinateTerminal),
    Grid(CoordinateGrid),
}

/// Zig: `Coordinate.Tag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoordinateTag {
    Surface,
    Terminal,
    Grid,
}

/// Zig: `Coordinate.Surface`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CoordinateSurface {
    pub x: f64,
    pub y: f64,
}

/// Zig: `Coordinate.Terminal`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CoordinateTerminal {
    pub x: f64,
    pub y: f64,
}

/// Zig: `Coordinate.Grid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CoordinateGrid {
    pub x: GridSizeUnit,
    pub y: GridSizeUnit,
}

impl Coordinate {
    /// The active tag of this coordinate.
    pub fn tag(self) -> CoordinateTag {
        match self {
            Coordinate::Surface(_) => CoordinateTag::Surface,
            Coordinate::Terminal(_) => CoordinateTag::Terminal,
            Coordinate::Grid(_) => CoordinateTag::Grid,
        }
    }

    /// Convert a coordinate to a different space within the same Size.
    pub fn convert(self, to: CoordinateTag, size: Size) -> Coordinate {
        // Unlikely fast-path but avoid work.
        if self.tag() == to {
            return self;
        }

        // To avoid the combinatorial explosion of conversion functions, we
        // convert to the surface system first and then reconvert from there.
        let surface = self.convert_to_surface(size);

        match to {
            CoordinateTag::Surface => Coordinate::Surface(surface),
            CoordinateTag::Terminal => Coordinate::Terminal(CoordinateTerminal {
                x: surface.x - size.padding.left as f64,
                y: surface.y - size.padding.top as f64,
            }),
            CoordinateTag::Grid => {
                // Get rid of the padding.
                let term = match Coordinate::Surface(surface).convert(CoordinateTag::Terminal, size)
                {
                    Coordinate::Terminal(t) => t,
                    _ => unreachable!(),
                };

                // We need our grid to clamp
                let grid = size.grid();

                // Calculate the grid position.
                let cell_width = size.cell.width as f64;
                let cell_height = size.cell.height as f64;
                let clamped_x = term.x.max(0.0);
                let clamped_y = term.y.max(0.0);
                let col = (clamped_x / cell_width) as GridSizeUnit;
                let row = (clamped_y / cell_height) as GridSizeUnit;
                let clamped_col = col.min(grid.columns - 1);
                let clamped_row = row.min(grid.rows - 1);
                Coordinate::Grid(CoordinateGrid {
                    x: clamped_col,
                    y: clamped_row,
                })
            }
        }
    }

    /// Convert a coordinate to the surface coordinate system.
    fn convert_to_surface(self, size: Size) -> CoordinateSurface {
        match self {
            Coordinate::Surface(v) => v,
            Coordinate::Terminal(v) => CoordinateSurface {
                x: v.x + size.padding.left as f64,
                y: v.y + size.padding.top as f64,
            },
            Coordinate::Grid(v) => {
                let col = v.x as f64;
                let row = v.y as f64;
                let cell_width = size.cell.width as f64;
                let cell_height = size.cell.height as f64;
                let padding_left = size.padding.left as f64;
                let padding_top = size.padding.top as f64;
                CoordinateSurface {
                    x: col * cell_width + padding_left,
                    y: row * cell_height + padding_top,
                }
            }
        }
    }
}

/// The dimensions of a single "cell" in the terminal grid.
///
/// The dimensions are dependent on the current loaded set of font glyphs.
/// We calculate the width based on the widest character and the height based
/// on the height requirement for an underscore (the "lowest" -- visually --
/// character).
///
/// The units for the width and height are in world space. They have to
/// be normalized for any renderer implementation.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellSize {
    pub width: u32,
    pub height: u32,
}

/// The dimensions of the screen that the grid is rendered to. This is the
/// terminal screen, so it is likely a subset of the window size. The dimensions
/// should be in pixels.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScreenSize {
    pub width: u32,
    pub height: u32,
}

impl ScreenSize {
    /// Subtract padding from the screen size.
    pub fn sub_padding(self, padding: Padding) -> ScreenSize {
        ScreenSize {
            width: self.width.saturating_sub(padding.left + padding.right),
            height: self.height.saturating_sub(padding.top + padding.bottom),
        }
    }

    /// Calculates the amount of blank space around the grid. This is possible
    /// when padding isn't balanced.
    ///
    /// The "self" screen size here should be the unpadded screen.
    pub fn blank_padding(self, padding: Padding, grid: GridSize, cell: CellSize) -> Padding {
        let grid_width = grid.columns as u32 * cell.width;
        let grid_height = grid.rows as u32 * cell.height;
        let padded_width = grid_width + (padding.left + padding.right);
        let padded_height = grid_height + (padding.top + padding.bottom);

        // Note these have to use a saturating subtraction to avoid underflow
        // because our padding can cause the padded sizes to be larger than
        // our real screen if the screen is shrunk to a minimal size such
        // as 1x1.
        let leftover_width = self.width.saturating_sub(padded_width);
        let leftover_height = self.height.saturating_sub(padded_height);

        Padding {
            top: 0,
            bottom: leftover_height,
            right: leftover_width,
            left: 0,
        }
    }

    /// Returns true if two sizes are equal.
    pub fn equals(self, other: ScreenSize) -> bool {
        self.width == other.width && self.height == other.height
    }
}

/// Zig: `GridSize.Unit`.
pub type GridSizeUnit = terminal_size::CellCountInt;

/// The dimensions of the grid itself, in rows/columns units.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GridSize {
    pub columns: GridSizeUnit,
    pub rows: GridSizeUnit,
}

impl GridSize {
    /// Initialize a grid size based on a screen and cell size.
    pub fn init(screen: ScreenSize, cell: CellSize) -> GridSize {
        let mut result = GridSize::default();
        result.update(screen, cell);
        result
    }

    /// Update the columns/rows for the grid based on the given screen and
    /// cell size.
    pub fn update(&mut self, screen: ScreenSize, cell: CellSize) {
        let cell_width = cell.width as f32;
        let cell_height = cell.height as f32;
        let screen_width = screen.width as f32;
        let screen_height = screen.height as f32;
        let calc_cols = (screen_width / cell_width) as GridSizeUnit;
        let calc_rows = (screen_height / cell_height) as GridSizeUnit;
        self.columns = calc_cols.max(1);
        self.rows = calc_rows.max(1);
    }

    /// Returns true if two sizes are equal.
    pub fn equals(self, other: GridSize) -> bool {
        self.columns == other.columns && self.rows == other.rows
    }
}

/// The padding to add to a screen.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Padding {
    pub top: u32,
    pub bottom: u32,
    pub right: u32,
    pub left: u32,
}

impl Padding {
    /// Returns padding that balances the whitespace around the screen
    /// for the given grid and cell sizes.
    pub fn balanced(screen: ScreenSize, grid: GridSize, cell: CellSize) -> Padding {
        // Turn our cell sizes into floats for the math
        let cell_width = cell.width as f32;
        let cell_height = cell.height as f32;

        // The size of our full grid
        let grid_width = grid.columns as f32 * cell_width;
        let grid_height = grid.rows as f32 * cell_height;

        // The empty space to the right of a line and bottom of the last row
        let space_right = screen.width as f32 - grid_width;
        let space_bot = screen.height as f32 - grid_height;

        // The padding is split equally along both axes.
        let padding_right = (space_right / 2.0).floor();
        let padding_left = padding_right;

        let padding_bot = (space_bot / 2.0).floor();
        let padding_top = padding_bot;

        let zero = 0f32;
        Padding {
            top: zero.max(padding_top) as u32,
            bottom: zero.max(padding_bot) as u32,
            right: zero.max(padding_right) as u32,
            left: zero.max(padding_left) as u32,
        }
    }

    /// Add another padding to this one
    pub fn add(self, other: Padding) -> Padding {
        Padding {
            top: self.top + other.top,
            bottom: self.bottom + other.bottom,
            right: self.right + other.right,
            left: self.left + other.left,
        }
    }

    /// Equality test between two paddings.
    pub fn eql(self, other: Padding) -> bool {
        self.top == other.top
            && self.bottom == other.bottom
            && self.right == other.right
            && self.left == other.left
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_balance_padding_equal_distributes_whitespace_equally() {
        // screen=1050x850, cell=10x20, explicit=4 each side
        // grid: (1050-8)/10=104 cols, (850-8)/20=42 rows
        // leftover: 1050-1040=10 horizontal, 850-840=10 vertical
        // balanced: left=right=5, top=bottom=5
        let mut size = Size {
            screen: ScreenSize {
                width: 1050,
                height: 850,
            },
            cell: CellSize {
                width: 10,
                height: 20,
            },
            padding: Padding::default(),
        };
        size.balance_padding(
            Padding {
                top: 4,
                bottom: 4,
                left: 4,
                right: 4,
            },
            PaddingBalance::Equal,
        );
        assert_eq!(size.padding.left, size.padding.right);
        assert_eq!(size.padding.top, size.padding.bottom);
        assert!(size.padding.top > 0);
    }

    #[test]
    fn size_balance_padding_true_shifts_excess_top_to_bottom() {
        // screen=1090x1070, cell=20x40, explicit=0
        // grid: 1090/20=54 cols, 1070/40=26 rows
        // leftover: 1090-1080=10, 1070-1040=30
        // balanced: left=right=5, top=bottom=15
        // vshift cap: (0+0+20)/2=10, vshift=15-10=5
        // result: top=10, bottom=20
        let mut size = Size {
            screen: ScreenSize {
                width: 1090,
                height: 1070,
            },
            cell: CellSize {
                width: 20,
                height: 40,
            },
            padding: Padding::default(),
        };
        size.balance_padding(Padding::default(), PaddingBalance::True);
        assert_eq!(size.padding.left, size.padding.right);
        assert!(size.padding.top < size.padding.bottom);
        assert_eq!(10u32, size.padding.top);
        assert_eq!(20u32, size.padding.bottom);
    }

    #[test]
    fn padding_balanced_on_zero() {
        // On some systems, our screen can be zero-sized for a bit, and we
        // don't want to end up with negative padding.
        let grid = GridSize {
            columns: 100,
            rows: 37,
        };
        let cell = CellSize {
            width: 10,
            height: 20,
        };
        let screen = ScreenSize {
            width: 0,
            height: 0,
        };
        let padding = Padding::balanced(screen, grid, cell);
        assert_eq!(Padding::default(), padding);
    }

    #[test]
    fn grid_size_update_exact() {
        let mut grid = GridSize::default();
        grid.update(
            ScreenSize {
                width: 100,
                height: 40,
            },
            CellSize {
                width: 5,
                height: 10,
            },
        );

        assert_eq!(20 as GridSizeUnit, grid.columns);
        assert_eq!(4 as GridSizeUnit, grid.rows);
    }

    #[test]
    fn grid_size_update_rounding() {
        let mut grid = GridSize::default();
        grid.update(
            ScreenSize {
                width: 20,
                height: 40,
            },
            CellSize {
                width: 6,
                height: 15,
            },
        );

        assert_eq!(3 as GridSizeUnit, grid.columns);
        assert_eq!(2 as GridSizeUnit, grid.rows);
    }

    #[test]
    fn coordinate_conversion() {
        // A size for testing purposes. Purposely easy to calculate numbers.
        let test_size = Size {
            screen: ScreenSize {
                width: 100,
                height: 100,
            },
            cell: CellSize {
                width: 5,
                height: 10,
            },
            padding: Padding::default(),
        };

        let grid = |x, y| Coordinate::Grid(CoordinateGrid { x, y });
        let surface = |x, y| Coordinate::Surface(CoordinateSurface { x, y });

        // Each pair is a test case of [expected, actual]. We only test
        // one-way conversion because conversion can be lossy due to clamping
        // and so on.
        let table: [[Coordinate; 2]; 5] = [
            [grid(0, 0), surface(0.0, 0.0)],
            [grid(1, 0), surface(6.0, 0.0)],
            [grid(1, 1), surface(6.0, 10.0)],
            [grid(0, 0), surface(-10.0, -10.0)],
            [
                grid(test_size.grid().columns - 1, test_size.grid().rows - 1),
                surface(100_000.0, 100_000.0),
            ],
        ];

        for pair in table {
            let expected = pair[0];
            let actual = pair[1].convert(expected.tag(), test_size);
            assert_eq!(expected, actual);
        }
    }
}
