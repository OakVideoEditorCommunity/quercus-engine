use super::*;

/// Standard palette: getColour picks host colours by enum (out of range ->
/// ErrValue).
#[test]
fn get_colour_palette() {
	let ctx = make_context((640.0, 480.0), (1.0, 1.0));
	let mut out = OfxRGBAColourF {
		r: 0.0,
		g: 0.0,
		b: 0.0,
		a: 0.0,
	};
	unsafe {
		assert_eq!(get_colour(ctx, std_colour::ACTIVE, &mut out), status::OK);
	}
	assert_eq!((out.r, out.g, out.b, out.a), (1.0, 0.84, 0.0, 1.0));
	// Out-of-range enum → ErrValue.
	unsafe {
		assert_eq!(get_colour(ctx, 99, &mut out), status::ERR_VALUE);
		assert_eq!(get_colour(ctx, -1, &mut out), status::ERR_VALUE);
		// Null output pointer → ErrValue.
		assert_eq!(
			get_colour(ctx, std_colour::BACKGROUND, std::ptr::null_mut()),
			status::ERR_VALUE
		);
	}
	drop_context(ctx);
}

/// setColour really writes state: after setLineWidth/setLineStipple it is
/// read back from the live table (the plugin's view during a draw
/// action).
#[test]
fn set_colour_and_line_state() {
	let ctx = make_context((100.0, 100.0), (2.0, 2.0));
	let col = OfxRGBAColourF {
		r: 0.9,
		g: 0.1,
		b: 0.2,
		a: 1.0,
	};
	unsafe {
		assert_eq!(set_colour(ctx, &col), status::OK);
		assert_eq!(set_line_width(ctx, 3.5), status::OK);
		assert_eq!(set_line_stipple(ctx, stipple::DASH), status::OK);
		assert_eq!(set_line_stipple(ctx, 99), status::ERR_VALUE);
	}

	let addr = ctx as usize;
	let live = lock();
	let c = live.get(&addr).unwrap();
	assert_eq!(c.colour, [0.9, 0.1, 0.2, 1.0]);
	assert_eq!(c.line_width, 3.5);
	assert_eq!(c.stipple, stipple::DASH);
	drop(live);
	drop_context(ctx);
}

/// Calling after removal -> Failed (the "outside kOfxInteractActionDraw"
/// semantics).
#[test]
fn draw_suite_outside_draw_fails() {
	let ctx = make_context((10.0, 10.0), (1.0, 1.0));
	drop_context(ctx);
	let col = OfxRGBAColourF {
		r: 1.0,
		g: 0.0,
		b: 0.0,
		a: 1.0,
	};
	let mut out = OfxRGBAColourF {
		r: 0.0,
		g: 0.0,
		b: 0.0,
		a: 0.0,
	};
	let s = std::ffi::CString::new("x").unwrap();
	let pos = OfxPointD { x: 0.0, y: 0.0 };
	let pts = [OfxPointD { x: 0.0, y: 0.0 }, OfxPointD { x: 10.0, y: 10.0 }];
	unsafe {
		assert_eq!(set_colour(ctx, &col), status::FAILED);
		assert_eq!(get_colour(ctx, std_colour::TEXT, &mut out), status::FAILED);
		assert_eq!(set_line_width(ctx, 1.0), status::FAILED);
		assert_eq!(set_line_stipple(ctx, stipple::SOLID), status::FAILED);
		// Valid arguments (avoiding the null-pointer checks) → resolve fails after removal → Failed.
		assert_eq!(
			draw(ctx, primitive::RECTANGLE, pts.as_ptr(), 2),
			status::FAILED
		);
		// drawText likewise Failed (resolve fails first).
		assert_eq!(draw_text(ctx, s.as_ptr(), &pos, 0), status::FAILED);
	}
}

/// draw primitive argument validation (does not touch GL): invalid
/// primitive/point count -> ErrValue.
#[test]
fn draw_primitive_argument_validation() {
	let ctx = make_context((10.0, 10.0), (1.0, 1.0));
	let pts = [OfxPointD { x: 0.0, y: 0.0 }, OfxPointD { x: 10.0, y: 10.0 }];
	unsafe {
		// Invalid primitive enum.
		assert_eq!(draw(ctx, 99, pts.as_ptr(), 2), status::ERR_VALUE);
		assert_eq!(draw(ctx, -1, pts.as_ptr(), 2), status::ERR_VALUE);
		// Invalid point count: odd count for LINES.
		assert_eq!(
			draw(ctx, primitive::LINES, pts.as_ptr(), 3),
			status::ERR_VALUE
		);
		// RECTANGLE needs exactly 2 points.
		assert_eq!(
			draw(ctx, primitive::RECTANGLE, pts.as_ptr(), 3),
			status::ERR_VALUE
		);
		// POLYGON needs >= 3 points.
		assert_eq!(
			draw(ctx, primitive::POLYGON, pts.as_ptr(), 2),
			status::ERR_VALUE
		);
		// Null pointer with a positive point count.
		assert_eq!(
			draw(ctx, primitive::LINES, std::ptr::null(), 2),
			status::ERR_VALUE
		);
		// Valid arguments: the GL path is environment-dependent (the host may
		// fail without a current context) — passing argument validation is
		// enough, the exact status is not asserted here.
		let st = draw(ctx, primitive::RECTANGLE, pts.as_ptr(), 2);
		assert!(
			st == status::OK || st == status::FAILED,
			"valid arguments should be OK (with GL) or Failed (no GL context), got {st}"
		);
	}
	drop_context(ctx);
}

/// drawText: valid arguments -> Unsupported (faithful refusal); null
/// pointer -> ErrValue.
#[test]
fn draw_text_is_honest_unsupported() {
	let ctx = make_context((10.0, 10.0), (1.0, 1.0));
	let s = std::ffi::CString::new("hello").unwrap();
	let pos = OfxPointD { x: 1.0, y: 2.0 };
	unsafe {
		assert_eq!(draw_text(ctx, s.as_ptr(), &pos, 0), status::ERR_UNSUPPORTED);
		assert_eq!(draw_text(ctx, std::ptr::null(), &pos, 0), status::ERR_VALUE);
	}
	drop_context(ctx);
}
