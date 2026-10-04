// fastgrid.zig — hot-path terminal grid primitives.
// built as a static lib: `zig build-lib fastgrid.zig -O ReleaseFast -fPIC`
// exposes C ABI symbols that rust calls via extern.

const std = @import("std");

// 12-byte C ABI cell. Hyperlink ids live in a parallel u32 grid on the Rust
// side so this stays compact without packed/unaligned fields.
pub const Cell = extern struct {
    glyph: u32 = ' ',
    fg: u16 = 7,
    bg: u16 = 0,
    attrs: u16 = 0,
    width: u8 = 1,
    _pad: u8 = 0,
};

comptime {
    std.debug.assert(@sizeOf(Cell) == 12);
}

// scroll a region up by `n` rows. the top `n` rows are discarded; the bottom
// `n` rows become blanks filled with the given cell.
// cells layout: row-major, `cols` wide, `rows` tall.
export fn fg_scroll_up(
    cells: [*]Cell,
    cols: u32,
    rows: u32,
    top: u32,
    bottom: u32,
    n: u32,
    blank: Cell,
) void {
    if (n == 0 or bottom <= top or bottom > rows) return;
    const region = bottom - top;
    const steps = if (n > region) region else n;

    const src_start = (top + steps) * cols;
    const dst_start = top * cols;
    const move_rows = region - steps;

    if (move_rows > 0) {
        const count = move_rows * cols;
        // overlapping move — zig's std.mem.copyForwards handles the direction
        var i: usize = 0;
        while (i < count) : (i += 1) {
            cells[dst_start + i] = cells[src_start + i];
        }
    }
    // fill blanks at the bottom of the region
    const fill_start = (top + move_rows) * cols;
    const fill_count = steps * cols;
    var j: usize = 0;
    while (j < fill_count) : (j += 1) {
        cells[fill_start + j] = blank;
    }
}

// scroll a region down by `n` rows. the bottom `n` rows are discarded; the top
// `n` rows become blanks.
export fn fg_scroll_down(
    cells: [*]Cell,
    cols: u32,
    rows: u32,
    top: u32,
    bottom: u32,
    n: u32,
    blank: Cell,
) void {
    if (n == 0 or bottom <= top or bottom > rows) return;
    const region = bottom - top;
    const steps = if (n > region) region else n;
    const move_rows = region - steps;

    if (move_rows > 0) {
        // iterate from the end to avoid overlap corruption
        var i: usize = move_rows * cols;
        while (i > 0) {
            i -= 1;
            const src = top * cols + i;
            const dst = (top + steps) * cols + i;
            cells[dst] = cells[src];
        }
    }
    const fill_count = steps * cols;
    var j: usize = 0;
    while (j < fill_count) : (j += 1) {
        cells[top * cols + j] = blank;
    }
}

// clear a cell-rectangle to a given blank cell.
export fn fg_clear_rect(
    cells: [*]Cell,
    cols: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    blank: Cell,
) void {
    var row: u32 = 0;
    while (row < h) : (row += 1) {
        var col: u32 = 0;
        while (col < w) : (col += 1) {
            cells[(y + row) * cols + x + col] = blank;
        }
    }
}

// insert `n` blank cells at (x,y), shifting the rest of the row right.
// cells pushed off the right edge are lost.
export fn fg_insert_cells(
    cells: [*]Cell,
    cols: u32,
    x: u32,
    y: u32,
    n: u32,
    blank: Cell,
) void {
    if (n == 0 or x >= cols) return;
    const row_start = y * cols;
    const shift = if (x + n >= cols) cols - x else n;
    var i: usize = cols;
    while (i > x + shift) {
        i -= 1;
        cells[row_start + i] = cells[row_start + i - shift];
    }
    var j: u32 = 0;
    while (j < shift) : (j += 1) {
        cells[row_start + x + j] = blank;
    }
}

// delete `n` cells at (x,y), shifting the rest of the row left; the right edge
// gets filled with blanks.
export fn fg_delete_cells(
    cells: [*]Cell,
    cols: u32,
    x: u32,
    y: u32,
    n: u32,
    blank: Cell,
) void {
    if (n == 0 or x >= cols) return;
    const row_start = y * cols;
    const shift = if (x + n >= cols) cols - x else n;
    const move = cols - x - shift;
    var i: usize = 0;
    while (i < move) : (i += 1) {
        cells[row_start + x + i] = cells[row_start + x + i + shift];
    }
    var j: u32 = 0;
    while (j < shift) : (j += 1) {
        cells[row_start + cols - shift + j] = blank;
    }
}

// sanity: size this lib expects for `Cell`. rust must assert equality at init.
export fn fg_cell_size() usize {
    return @sizeOf(Cell);
}
