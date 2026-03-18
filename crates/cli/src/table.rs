/// Print a formatted table with auto-calculated column widths.
///
/// `right_align` contains column indices that should be right-aligned.
/// An optional `markers` slice provides a per-row prefix (e.g. "* " for active items).
pub fn print_table(
    headers: &[&str],
    rows: &[Vec<String>],
    right_align: &[usize],
    markers: Option<&[&str]>,
) {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.len());
            }
        }
    }
    for w in widths.iter_mut() {
        *w += 2;
    }

    // Header prefix spacing (match marker width if markers are used)
    let prefix_width = markers
        .and_then(|m| m.iter().map(|s| s.len()).max())
        .unwrap_or(0);
    if prefix_width > 0 {
        print!("{:width$}", "", width = prefix_width);
    }
    for (i, h) in headers.iter().enumerate() {
        if right_align.contains(&i) {
            print!("{:>width$}", h, width = widths[i]);
        } else {
            print!("{:<width$}", h, width = widths[i]);
        }
    }
    println!();

    for (row_idx, row) in rows.iter().enumerate() {
        if let Some(m) = markers {
            print!("{}", m.get(row_idx).unwrap_or(&""));
        }
        for (i, cell) in row.iter().enumerate() {
            let w = widths.get(i).copied().unwrap_or(0);
            if right_align.contains(&i) {
                print!("{:>width$}", cell, width = w);
            } else {
                print!("{:<width$}", cell, width = w);
            }
        }
        println!();
    }
}
