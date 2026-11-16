use super::*;
use crate::node::NodeBehavior;
use crate::value::{NodeValueTable, ValueType};
use quercus_core::Rational;

/// A `Tm` with just the calendar fields set (rest zeroed).
fn tm(sec: i32, min: i32, hour: i32, mday: i32, mon: i32, year: i32) -> Tm {
	Tm {
		tm_sec: sec,
		tm_min: min,
		tm_hour: hour,
		tm_mday: mday,
		tm_mon: mon,
		tm_year: year,
		..unsafe { std::mem::zeroed() }
	}
}

#[test]
fn input_names() {
	let n = TimeFormatNode;
	assert_eq!(n.input_name(TIME_INPUT), "Time");
	assert_eq!(n.input_name(FORMAT_INPUT), "Format");
	assert_eq!(
		n.input_name(LOCAL_TIME_INPUT),
		"Interpret time as local time"
	);
	assert_eq!(n.input_name("other_in"), "other_in");
}

#[test]
fn create_wires_inputs() {
	let (core, behavior) = create();
	assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.timeformat");
	assert_eq!(
		core.get_input(TIME_INPUT).unwrap().value_type,
		ValueType::Float
	);
	assert_eq!(
		core.get_input(FORMAT_INPUT).unwrap().value_type,
		ValueType::Text
	);
	assert_eq!(
		core.get_input(FORMAT_INPUT).unwrap().default,
		NodeValue::Text("hh:mm:ss".to_string())
	);
	assert_eq!(
		core.get_input(LOCAL_TIME_INPUT).unwrap().value_type,
		ValueType::Boolean
	);
}

#[test]
fn format_24_hour_clock_without_am_pm_token() {
	let t = tm(6, 5, 13, 9, 7, 124); // 2024-08-09 13:05:06
	assert_eq!(format_date_time(&t, 0, "hh:mm:ss"), "13:05:06");
	assert_eq!(format_date_time(&t, 0, "HH"), "13");
}

#[test]
fn format_12_hour_clock_with_am_pm_token() {
	let pm = tm(0, 0, 13, 1, 0, 124);
	assert_eq!(format_date_time(&pm, 0, "h:mm AP"), "1:00 PM");
	assert_eq!(format_date_time(&pm, 0, "h:mm ap"), "1:00 pm");
	assert_eq!(format_date_time(&pm, 0, "h:mm A"), "1:00 PM");
	assert_eq!(format_date_time(&pm, 0, "h:mm a"), "1:00 pm");
	let am = tm(0, 0, 0, 1, 0, 124);
	assert_eq!(format_date_time(&am, 0, "h AP"), "12 AM");
	let midnight = tm(0, 0, 0, 1, 0, 124);
	assert_eq!(format_date_time(&midnight, 0, "hh AP"), "12 AM");
}

#[test]
fn format_date_tokens() {
	let t = tm(6, 5, 13, 9, 7, 124); // 2024-08-09 13:05:06
	assert_eq!(format_date_time(&t, 0, "yyyy-MM-dd"), "2024-08-09");
	assert_eq!(format_date_time(&t, 0, "yy-M-d"), "24-8-9");
	assert_eq!(format_date_time(&t, 0, "yyyy"), "2024");
}

#[test]
fn format_millisecond_tokens() {
	let t = tm(0, 0, 0, 1, 0, 124);
	assert_eq!(format_date_time(&t, 5, "z"), "5");
	assert_eq!(format_date_time(&t, 5, "zz"), "5");
	assert_eq!(format_date_time(&t, 5, "zzz"), "005");
	assert_eq!(format_date_time(&t, 123, "zzz"), "123");
}

#[test]
fn format_literal_sections() {
	let t = tm(0, 0, 13, 9, 7, 124);
	assert_eq!(format_date_time(&t, 0, "'Literal text'"), "Literal text");
	assert_eq!(format_date_time(&t, 0, "'It''s' HH"), "Its 13");
	assert_eq!(
		format_date_time(&t, 0, "yyyy'unterminated"),
		"2024unterminated"
	);
}

#[test]
fn format_unknown_tokens_copied_verbatim() {
	let t = tm(0, 0, 13, 9, 7, 124);
	assert_eq!(format_date_time(&t, 0, "Q % 5"), "Q % 5");
}

#[test]
fn value_formats_utc_epoch() {
	let (mut core, behavior) = create();
	// 12:34:56 on 1970-01-01 UTC = 45296 seconds.
	core.set_standard_value(TIME_INPUT, -1, NodeValue::Float(45296.0));
	core.set_standard_value(
		FORMAT_INPUT,
		-1,
		NodeValue::Text("yyyy-MM-dd HH:mm:ss".to_string()),
	);
	let mut table = NodeValueTable::default();
	behavior.value(
		&core,
		&crate::value::NodeValueRow::default(),
		Rational::new(0, 1),
		&mut table,
	);
	assert_eq!(
		table.get(ValueType::Text),
		Some(&NodeValue::Text("1970-01-01 12:34:56".to_string()))
	);
}

#[test]
fn value_formats_milliseconds_utc() {
	let (mut core, behavior) = create();
	// Same instant plus 789 ms.
	core.set_standard_value(TIME_INPUT, -1, NodeValue::Float(45296.789));
	core.set_standard_value(
		FORMAT_INPUT,
		-1,
		NodeValue::Text("HH:mm:ss.zzz".to_string()),
	);
	let mut table = NodeValueTable::default();
	behavior.value(
		&core,
		&crate::value::NodeValueRow::default(),
		Rational::new(0, 1),
		&mut table,
	);
	assert_eq!(
		table.get(ValueType::Text),
		Some(&NodeValue::Text("12:34:56.789".to_string()))
	);
}

#[test]
fn value_uses_connected_inputs() {
	let (core, behavior) = create();
	let mut row = crate::value::NodeValueRow::default();
	row.insert(TIME_INPUT.to_string(), NodeValue::Float(45296.0));
	row.insert(
		FORMAT_INPUT.to_string(),
		NodeValue::Text("yyyy".to_string()),
	);
	row.insert(LOCAL_TIME_INPUT.to_string(), NodeValue::Boolean(false));
	let mut table = NodeValueTable::default();
	behavior.value(&core, &row, Rational::new(0, 1), &mut table);
	assert_eq!(
		table.get(ValueType::Text),
		Some(&NodeValue::Text("1970".to_string()))
	);
}

#[test]
fn value_localtime_flag_routes_to_localtime_r() {
	let (mut core, behavior) = create();
	core.set_standard_value(TIME_INPUT, -1, NodeValue::Float(45296.0));
	core.set_standard_value(FORMAT_INPUT, -1, NodeValue::Text("yyyy".to_string()));

	// Expected values computed through the same C library calls the C++
	// makes — this validates the routing (which function is called for
	// each flag value), not the C library itself.
	let secs: c_long = 45296;
	let local = break_down_time(secs, true);
	let utc = break_down_time(secs, false);
	let local_expected = format!("{:04}", local.tm_year + 1900);
	let utc_expected = format!("{:04}", utc.tm_year + 1900);
	assert_eq!(utc_expected, "1970");

	core.set_standard_value(LOCAL_TIME_INPUT, -1, NodeValue::Boolean(true));
	let mut table = NodeValueTable::default();
	behavior.value(
		&core,
		&crate::value::NodeValueRow::default(),
		Rational::new(0, 1),
		&mut table,
	);
	assert_eq!(
		table.get(ValueType::Text),
		Some(&NodeValue::Text(local_expected))
	);

	core.set_standard_value(LOCAL_TIME_INPUT, -1, NodeValue::Boolean(false));
	let mut table = NodeValueTable::default();
	behavior.value(
		&core,
		&crate::value::NodeValueRow::default(),
		Rational::new(0, 1),
		&mut table,
	);
	assert_eq!(
		table.get(ValueType::Text),
		Some(&NodeValue::Text(utc_expected))
	);
}

#[test]
fn duplicate_copies_node() {
	let (_core, behavior) = create();
	let copy = behavior.duplicate(&_core).unwrap();
	assert_eq!(copy.type_id(), "org.olivevideoeditor.Olive.timeformat");
	assert_eq!(copy.name(), "Time Format");
}
