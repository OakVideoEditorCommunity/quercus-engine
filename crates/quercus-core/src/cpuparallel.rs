// Quercus Video Editor - Non-Linear Video Editor
// Copyright (C) 2026 Quercus Team
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Scoped-thread helpers for CPU-only media and pixel work.

/// Maximum workers spawned by one CPU operation. Render jobs and codec
/// instances can themselves run concurrently, so unbounded per-frame
/// fan-out would oversubscribe the machine.
const MAX_WORKERS: usize = 8;

/// Minimum logical units (usually pixels) before spawning is worthwhile.
const MIN_PARALLEL_UNITS: usize = 1 << 19;

/// CPU worker budget for codecs and large pixel operations.
pub fn thread_count() -> usize {
	std::thread::available_parallelism()
		.map(|n| n.get())
		.unwrap_or(1)
		.min(MAX_WORKERS)
}

fn workers_for(units: usize) -> usize {
	if units >= MIN_PARALLEL_UNITS {
		thread_count()
	} else {
		1
	}
}

/// Run `f` over aligned bands of `buf`, using scoped threads for large inputs.
///
/// `align` is the element count of one logical unit (4 for F32 RGBA, 16 for
/// byte RGBA), so a band never splits a unit. Small inputs stay on the caller
/// thread. Each band can have a different length.
pub fn for_each_band<T: Send>(buf: &mut [T], align: usize, f: impl Fn(&mut [T]) + Sync) {
	assert!(align > 0, "band alignment must be non-zero");
	let workers = workers_for(buf.len() / align);
	if workers <= 1 {
		f(buf);
		return;
	}
	let band_len = (buf.len() / workers).div_ceil(align) * align;
	let f = &f;
	std::thread::scope(|scope| {
		for band in buf.chunks_mut(band_len) {
			scope.spawn(move || f(band));
		}
	});
}

/// Run `f` over logical rows, using scoped threads for large frames.
///
/// `row_stride` is the distance between rows in `T` elements and
/// `units_per_row` measures the work in one row (normally its pixel count).
/// The final row may be shorter than `row_stride`; rows may include padding.
/// `f` receives the absolute row index and that row's mutable slice.
pub fn for_each_row<T: Send>(
	buf: &mut [T],
	row_stride: usize,
	rows: usize,
	units_per_row: usize,
	f: impl Fn(usize, &mut [T]) + Sync,
) {
	if row_stride == 0 || rows == 0 {
		return;
	}
	let workers = workers_for(rows.saturating_mul(units_per_row)).min(rows);
	let f = &f;
	if workers <= 1 {
		for (row_index, row) in buf.chunks_mut(row_stride).take(rows).enumerate() {
			f(row_index, row);
		}
		return;
	}
	let rows_per_band = rows.div_ceil(workers);
	let band_stride = row_stride.saturating_mul(rows_per_band);
	std::thread::scope(|scope| {
		for (band_index, band) in buf.chunks_mut(band_stride).enumerate() {
			scope.spawn(move || {
				for (local_index, row) in band.chunks_mut(row_stride).enumerate() {
					let row_index = band_index * rows_per_band + local_index;
					if row_index < rows {
						f(row_index, row);
					}
				}
			});
		}
	});
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::collections::HashSet;
	use std::sync::Mutex;

	#[test]
	fn large_rows_use_scoped_threads_without_splitting_rows() {
		if thread_count() < 2 {
			return;
		}
		let rows = 2048;
		let units_per_row = 512;
		let row_stride = units_per_row + 7;
		let mut data = vec![0usize; rows * row_stride];
		let threads = Mutex::new(HashSet::new());
		for_each_row(
			&mut data,
			row_stride,
			rows,
			units_per_row,
			|row_index, row| {
				threads
					.lock()
					.unwrap_or_else(|e| e.into_inner())
					.insert(std::thread::current().id());
				row[..units_per_row].fill(row_index + 1);
			},
		);
		assert!(threads.lock().unwrap().len() > 1);
		for row_index in 0..rows {
			let row = &data[row_index * row_stride..(row_index + 1) * row_stride];
			assert!(row[..units_per_row].iter().all(|&v| v == row_index + 1));
			assert!(row[units_per_row..].iter().all(|&v| v == 0));
		}
	}

	#[test]
	fn small_rows_and_empty_input_stay_valid() {
		let mut data = vec![0u8; 12];
		for_each_row(&mut data, 4, 3, 2, |row_index, row| {
			row.fill(row_index as u8 + 1);
		});
		assert_eq!(data, [1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3]);

		let mut empty: [u8; 0] = [];
		for_each_row(&mut empty, 4, 0, 2, |_, _| unreachable!());
		for_each_band(&mut empty, 4, |band| assert!(band.is_empty()));
	}
}
