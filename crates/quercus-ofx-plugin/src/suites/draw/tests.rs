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
	// 越界枚举 → ErrValue。
	unsafe {
		assert_eq!(get_colour(ctx, 99, &mut out), status::ERR_VALUE);
		assert_eq!(get_colour(ctx, -1, &mut out), status::ERR_VALUE);
		// 空输出指针 → ErrValue。
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
		// 合法参数（避开空指针检查）→ 摘除后 resolve 失败 → Failed。
		assert_eq!(
			draw(ctx, primitive::RECTANGLE, pts.as_ptr(), 2),
			status::FAILED
		);
		// drawText 同样 Failed（先 resolve 失败）。
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
		// 非法原语枚举。
		assert_eq!(draw(ctx, 99, pts.as_ptr(), 2), status::ERR_VALUE);
		assert_eq!(draw(ctx, -1, pts.as_ptr(), 2), status::ERR_VALUE);
		// 点数不合法：LINES 奇数点。
		assert_eq!(
			draw(ctx, primitive::LINES, pts.as_ptr(), 3),
			status::ERR_VALUE
		);
		// RECTANGLE 需恰好 2 点。
		assert_eq!(
			draw(ctx, primitive::RECTANGLE, pts.as_ptr(), 3),
			status::ERR_VALUE
		);
		// POLYGON 需 ≥3 点。
		assert_eq!(
			draw(ctx, primitive::POLYGON, pts.as_ptr(), 2),
			status::ERR_VALUE
		);
		// 空指针 + 正点数。
		assert_eq!(
			draw(ctx, primitive::LINES, std::ptr::null(), 2),
			status::ERR_VALUE
		);
		// 合法参数：环境相关的 GL 路径（无 current 上下文时宿主可能
		// 失败）——参数校验已过即可，具体状态不在此断言。
		let st = draw(ctx, primitive::RECTANGLE, pts.as_ptr(), 2);
		assert!(
			st == status::OK || st == status::FAILED,
			"合法参数应 OK（有 GL）或 Failed（无 GL 上下文），got {st}"
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
