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
    if (cols == 0 or n == 0 or bottom <= top or bottom > rows) return;
    const region = bottom - top;
    const steps = if (n > region) region else n;

    const src_start: usize = @as(usize, top + steps) * @as(usize, cols);
    const dst_start: usize = @as(usize, top) * @as(usize, cols);
    const move_rows = region - steps;

    if (move_rows > 0) {
        const count: usize = @as(usize, move_rows) * @as(usize, cols);
        // Destination precedes source, so forward overlap-safe copying is correct.
        std.mem.copyForwards(Cell, cells[dst_start .. dst_start + count], cells[src_start .. src_start + count]);
    }
    // fill blanks at the bottom of the region
    const fill_start: usize = @as(usize, top + move_rows) * @as(usize, cols);
    const fill_count: usize = @as(usize, steps) * @as(usize, cols);
    @memset(cells[fill_start .. fill_start + fill_count], blank);
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
    if (cols == 0 or n == 0 or bottom <= top or bottom > rows) return;
    const region = bottom - top;
    const steps = if (n > region) region else n;
    const move_rows = region - steps;

    if (move_rows > 0) {
        const count: usize = @as(usize, move_rows) * @as(usize, cols);
        const src_start: usize = @as(usize, top) * @as(usize, cols);
        const dst_start: usize = @as(usize, top + steps) * @as(usize, cols);
        // Destination follows source, so copy backwards to preserve overlap.
        std.mem.copyBackwards(Cell, cells[dst_start .. dst_start + count], cells[src_start .. src_start + count]);
    }
    const fill_start: usize = @as(usize, top) * @as(usize, cols);
    const fill_count: usize = @as(usize, steps) * @as(usize, cols);
    @memset(cells[fill_start .. fill_start + fill_count], blank);
}

// clear a cell-rectangle to a given blank cell.
export fn fg_clear_rect(
    cells: [*]Cell,
    cols: u32,
    rows: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    blank: Cell,
) void {
    if (cols == 0 or rows == 0 or x >= cols or y >= rows or w == 0 or h == 0) return;
    const width = @min(w, cols - x);
    const height = @min(h, rows - y);
    var row: u32 = 0;
    while (row < height) : (row += 1) {
        const start: usize = @as(usize, y + row) * @as(usize, cols) + @as(usize, x);
        @memset(cells[start .. start + @as(usize, width)], blank);
    }
}

// insert `n` blank cells at (x,y), shifting the rest of the row right.
// cells pushed off the right edge are lost.
export fn fg_insert_cells(
    cells: [*]Cell,
    cols: u32,
    rows: u32,
    x: u32,
    y: u32,
    n: u32,
    blank: Cell,
) void {
    if (cols == 0 or y >= rows or n == 0 or x >= cols) return;
    const row_start: usize = @as(usize, y) * @as(usize, cols);
    const shift = @min(n, cols - x);
    const move = cols - x - shift;
    if (move > 0) {
        const src_start = row_start + @as(usize, x);
        const dst_start = src_start + @as(usize, shift);
        std.mem.copyBackwards(Cell, cells[dst_start .. dst_start + @as(usize, move)], cells[src_start .. src_start + @as(usize, move)]);
    }
    const fill_start = row_start + @as(usize, x);
    @memset(cells[fill_start .. fill_start + @as(usize, shift)], blank);
}

// delete `n` cells at (x,y), shifting the rest of the row left; the right edge
// gets filled with blanks.
export fn fg_delete_cells(
    cells: [*]Cell,
    cols: u32,
    rows: u32,
    x: u32,
    y: u32,
    n: u32,
    blank: Cell,
) void {
    if (cols == 0 or y >= rows or n == 0 or x >= cols) return;
    const row_start: usize = @as(usize, y) * @as(usize, cols);
    const shift = @min(n, cols - x);
    const move = cols - x - shift;
    if (move > 0) {
        const dst_start = row_start + @as(usize, x);
        const src_start = dst_start + @as(usize, shift);
        std.mem.copyForwards(Cell, cells[dst_start .. dst_start + @as(usize, move)], cells[src_start .. src_start + @as(usize, move)]);
    }
    const fill_start = row_start + @as(usize, cols - shift);
    @memset(cells[fill_start .. fill_start + @as(usize, shift)], blank);
}

// sanity: size this lib expects for `Cell`. rust must assert equality at init.
export fn fg_cell_size() usize {
    return @sizeOf(Cell);
}
