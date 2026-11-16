use super::*;

// ---- Control-plane protocol ------------------------------------------

#[test]
fn handshake_wire_format_matches_cpp_field_names() {
	let hs = HandshakeMsg {
		protocol_version: 1,
		shm_key: "olive-rw-1234-0-out".into(),
		input_shm_key: "".into(),
		input_slots: 0,
		output_slots: 6,
		slot_data_bytes: 4096,
		input_slot_data_bytes: 0,
	};
	let value = hs.to_json();
	// Key order is not part of the contract (JSON objects; the C++
	// QJsonObject is hash-ordered too), but the names must match the
	// C++ serializer exactly.
	assert_eq!(value["type"], "handshake");
	assert_eq!(value["protocol_version"], 1);
	assert_eq!(value["shm_key"], "olive-rw-1234-0-out");
	assert_eq!(value["input_shm_key"], "");
	assert_eq!(value["input_slots"], 0);
	assert_eq!(value["output_slots"], 6);
	assert_eq!(value["slot_data_bytes"], 4096);
	assert_eq!(value["input_slot_data_bytes"], 0);
	// And the serialized line must parse back to the same object.
	let round: serde_json::Value =
		serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap();
	assert_eq!(round, value);
}

#[test]
fn plugin_progress_message_round_trips() {
	let msg = PluginProgressMsg {
		label: "render".into(),
		message: "pass 1".into(),
		fraction: 0.5,
	};
	let value = msg.to_json();
	assert_eq!(value["type"], TYPE_PLUGIN_PROGRESS);
	assert_eq!(value["label"], "render");
	assert_eq!(value["message"], "pass 1");
	assert_eq!(value["fraction"], 0.5);

	// The NDJSON line parses back to the same payload (the wire
	// contract the worker writes and the dispatcher reads).
	let line = serde_json::to_string(&value).unwrap();
	let parsed: PluginProgressMsg = serde_json::from_str(&line).unwrap();
	assert_eq!(parsed, msg);
}

#[test]
fn plugin_progress_defaults_on_missing_fields() {
	// A minimal message (e.g. progressEnd forwarded as fraction 1.0
	// without touching label/message) deserializes cleanly.
	let m: PluginProgressMsg =
		serde_json::from_str(r#"{"type":"plugin_progress","fraction":1.0}"#).unwrap();
	assert_eq!(m.fraction, 1.0);
	assert!(m.label.is_empty());
	assert!(m.message.is_empty());
}

#[test]
fn plugin_cancel_wire_shape() {
	let value = plugin_cancel_json();
	assert_eq!(value["type"], TYPE_PLUGIN_CANCEL);
}

// ---- M3 OFX-host job protocol ---------------------------------------

#[test]
fn ofx_job_wire_roundtrip() {
	let job = OfxJobMsg {
		job: 7,
		type_id: "org.quercus.test-plugin".into(),
		time: 0.5,
		effect_input_id: "Source".into(),
		inputs: vec![
			OfxInputRef {
				name: "Source".into(),
				slot: 3,
			},
			OfxInputRef {
				name: "Background".into(),
				slot: 4,
			},
		],
		values: vec![WireEffectParam {
			input: "gain".into(),
			value: WireNodeValue::Float(0.25),
		}],
		src_slot: Some(3),
	};
	let value = job.to_json();
	assert_eq!(value["type"], TYPE_OFX_JOB);
	assert_eq!(value["job"], 7);
	assert_eq!(value["type_id"], "org.quercus.test-plugin");
	assert_eq!(value["inputs"][0]["name"], "Source");
	assert_eq!(value["inputs"][0]["slot"], 3);
	assert_eq!(value["values"][0]["input"], "gain");
	assert_eq!(value["src_slot"], 3);

	// The typed form parses the same line (the type tag is ignored by
	// serde's default unknown-field handling).
	let parsed: OfxJobMsg = serde_json::from_value(value).unwrap();
	assert_eq!(parsed, job);

	// A src-less job omits `src_slot` (defaults to None).
	let bare: OfxJobMsg = serde_json::from_str(r#"{"job":1,"type_id":"x"}"#).unwrap();
	assert_eq!(bare.src_slot, None);
	assert!(bare.inputs.is_empty());
	assert!(bare.values.is_empty());
}

#[test]
fn ofx_result_wire_roundtrip() {
	let ok = OfxResultMsg {
		job: 7,
		slot: 2,
		error: String::new(),
	};
	let value = ok.to_json();
	assert_eq!(value["type"], TYPE_OFX_RESULT);
	assert_eq!(value["job"], 7);
	assert_eq!(value["slot"], 2);
	let parsed: OfxResultMsg = serde_json::from_value(value).unwrap();
	assert_eq!(parsed, ok);

	let failed = OfxResultMsg {
		job: 8,
		slot: -1,
		error: "plugin not found".into(),
	};
	let parsed: OfxResultMsg = serde_json::from_value(failed.to_json()).unwrap();
	assert_eq!(parsed, failed);
}

#[test]
fn ofx_protocol_type_constants_are_stable() {
	assert_eq!(TYPE_OFX_JOB, "ofx_job");
	assert_eq!(TYPE_OFX_RESULT, "ofx_result");
	assert_eq!(TYPE_PLUGIN_PROGRESS, "plugin_progress");
	assert_eq!(TYPE_PLUGIN_CANCEL, "plugin_cancel");
}

#[test]
fn render_frame_parse_accepts_cpp_field_names() {
	let json = r#"{"type":"render_frame","ticket":42,"node":"abcd","time_num":1,"time_den":24,"width":1920,"height":1080,"format":-1,"channels":0,"mode":0,"input_slot":-1,"input_slots":[],"has_color_transform":false,"color_output":"","color_view":"","color_look":""}"#;
	let m: RenderFrameMsg = serde_json::from_str(json).unwrap();
	assert_eq!(m.ticket, 42);
	assert_eq!(m.node, "abcd");
	assert_eq!(m.time_num, 1);
	assert_eq!(m.time_den, 24);
	assert_eq!(m.width, 1920);
	assert_eq!(m.input_slot, -1);
}

#[test]
fn render_frame_defaults_on_missing_fields() {
	// The C++ parser defaults missing fields (QJsonValue defaults);
	// serde(default) mirrors that.
	let m: RenderFrameMsg = serde_json::from_str(r#"{"type":"render_frame","ticket":7}"#).unwrap();
	assert_eq!(m.ticket, 7);
	assert_eq!(m.time_den, 0);
	assert!(m.node.is_empty());
	assert!(!m.has_color_transform);
}

#[test]
fn error_message_carries_ticket_only_when_nonzero() {
	assert_eq!(
		error_message("boom", None),
		json!({ "type": "error", "message": "boom" })
	);
	assert_eq!(
		error_message("boom", Some(0)),
		json!({ "type": "error", "message": "boom" })
	);
	assert_eq!(
		error_message("boom", Some(9)),
		json!({ "type": "error", "message": "boom", "ticket": 9 })
	);
}

#[test]
fn write_message_emits_one_json_line() {
	let mut buf = Vec::new();
	write_message(&mut buf, &json!({ "type": "shutdown" })).unwrap();
	assert_eq!(String::from_utf8(buf).unwrap(), "{\"type\":\"shutdown\"}\n");
}

// ---- Protocol v2 messages (M15 S1) ---------------------------------

#[test]
fn hello_caps_wire_roundtrip() {
	let caps = HelloCapsMsg {
		protocol_version: 1,
		formats: vec![4, SLOT_FORMAT_BGRA8],
		max_slot_bytes: 8_294_400,
	};
	let line = json!({
		"type": TYPE_HELLO_CAPS,
		"protocol_version": 1,
		"formats": [4, SLOT_FORMAT_BGRA8],
		"max_slot_bytes": 8_294_400i64,
	});
	// Parse the wire line (the `"type"` field is tolerated by serde's
	// default behavior of ignoring unknown fields).
	let parsed: HelloCapsMsg = serde_json::from_value(line.clone()).unwrap();
	assert_eq!(parsed, caps);
	// And the field names are the canonical wire names.
	let value = serde_json::to_value(&caps).unwrap();
	assert_eq!(value["protocol_version"], 1);
	assert_eq!(value["formats"], json!([4, SLOT_FORMAT_BGRA8]));
	assert_eq!(value["max_slot_bytes"], 8_294_400i64);
}

#[test]
fn render_batch_wire_roundtrip() {
	let batch = RenderBatchMsg {
		batch_id: 7,
		tickets: vec![
			BatchTicketSpec {
				ticket: 41,
				slot: 0,
				time_num: 3,
				time_den: 24,
				width: 320,
				height: 180,
				format: SLOT_FORMAT_BGRA8,
				channels: 4,
				footage_file: "a.mp4".into(),
				footage_stream: 0,
				montage: vec![],
				adjustments: vec![],
				viewer_node: 0,
				project_key: String::new(),
			},
			BatchTicketSpec {
				ticket: 42,
				slot: 1,
				time_num: 4,
				time_den: 24,
				width: 320,
				height: 180,
				format: 4,
				channels: 4,
				footage_file: String::new(),
				footage_stream: 0,
				montage: vec![WireMontageClip {
					filename: "b.mp4".into(),
					stream_index: 1,
					in_num: 0,
					in_den: 24,
					out_num: 48,
					out_den: 24,
					media_in_num: 10,
					media_in_den: 24,
					gain: 0.5,
					pan: -0.5,
					fade_in_num: 1,
					fade_in_den: 4,
					fade_out_num: 0,
					fade_out_den: 1,
					audio_effects: vec![WireMontageAudioEffect {
						type_id: "org.olivevideoeditor.Olive.volume".into(),
						enabled: true,
						params: vec![WireEffectParam {
							input: "volume_in".into(),
							value: WireNodeValue::Float(0.25),
						}],
					}],
					effects: vec![WireMontageEffect {
						type_id: "org.olivevideoeditor.Olive.opacity".into(),
						enabled: true,
						effect_input_id: "tex_in".into(),
						params: vec![WireEffectParam {
							input: "opacity_in".into(),
							value: WireNodeValue::Float(0.5),
						}],
					}],
				}],
				adjustments: vec![WireAdjustmentSpan {
					in_num: 0,
					in_den: 24,
					out_num: 48,
					out_den: 24,
					track_index: 1,
					effects: vec![WireMontageEffect {
						type_id: "org.olivevideoeditor.Olive.opacity".into(),
						enabled: true,
						effect_input_id: "tex_in".into(),
						params: vec![WireEffectParam {
							input: "opacity_in".into(),
							value: WireNodeValue::Float(0.5),
						}],
					}],
				}],
				viewer_node: 0,
				project_key: String::new(),
			},
		],
	};
	let value = serde_json::to_value(&batch).unwrap();
	assert_eq!(value["batch_id"], 7);
	assert_eq!(value["tickets"][0]["ticket"], 41);
	assert_eq!(value["tickets"][0]["slot"], 0);
	assert_eq!(value["tickets"][0]["format"], SLOT_FORMAT_BGRA8);
	assert_eq!(value["tickets"][1]["montage"][0]["filename"], "b.mp4");
	assert_eq!(value["tickets"][1]["montage"][0]["media_in_num"], 10);
	// The effect stack rides the clip as an additive v2 field.
	assert_eq!(
		value["tickets"][1]["montage"][0]["effects"][0]["type_id"],
		"org.olivevideoeditor.Olive.opacity"
	);
	// Per-clip audio post fields ride the same additive wire shape.
	assert_eq!(value["tickets"][1]["montage"][0]["pan"], -0.5);
	assert_eq!(value["tickets"][1]["montage"][0]["fade_in_num"], 1);
	assert_eq!(value["tickets"][1]["montage"][0]["fade_in_den"], 4);
	assert_eq!(
		value["tickets"][1]["montage"][0]["audio_effects"][0]["type_id"],
		"org.olivevideoeditor.Olive.volume"
	);
	assert_eq!(
		value["tickets"][1]["montage"][0]["audio_effects"][0]["params"][0]["value"],
		json!({ "t": "float", "v": 0.25 })
	);
	assert_eq!(
		value["tickets"][1]["montage"][0]["effects"][0]["params"][0]["value"],
		json!({ "t": "float", "v": 0.5 })
	);
	// The adjustment layer rides the ticket as an additive v2 field: its
	// boundary is a montage-list index, not a track number.
	assert_eq!(value["tickets"][1]["adjustments"][0]["track_index"], 1);
	assert_eq!(value["tickets"][1]["adjustments"][0]["out_num"], 48);
	assert_eq!(
		value["tickets"][1]["adjustments"][0]["effects"][0]["type_id"],
		"org.olivevideoeditor.Olive.opacity"
	);
	// The ticket-side conversions agree with the wire structs.
	let parsed: WireAdjustmentSpan =
		serde_json::from_value(value["tickets"][1]["adjustments"][0].clone()).unwrap();
	let span = adjustment_from_wire(&parsed);
	assert_eq!(span.in_time, quercus_core::Rational::new(0, 24));
	assert_eq!(span.out_time, quercus_core::Rational::new(48, 24));
	assert_eq!(span.track_index, 1);
	assert_eq!(
		span.effects[0].type_id,
		"org.olivevideoeditor.Olive.opacity"
	);
	assert_eq!(span.effects[0].effect_input_id.as_deref(), Some("tex_in"));
	assert_eq!(
		span.effects[0].params[0].1,
		quercus_node::value::NodeValue::Float(0.5)
	);
	// `Rational::new` normalizes, so the wire form of the parsed span
	// collapses 48/24 to 2/1.
	let wire = wire_adjustment_from(&span);
	assert_eq!(
		wire,
		WireAdjustmentSpan {
			in_num: 0,
			in_den: 1,
			out_num: 2,
			out_den: 1,
			track_index: 1,
			effects: parsed.effects.clone(),
		}
	);
	// Round-trip back to the struct.
	let round: RenderBatchMsg = serde_json::from_value(value).unwrap();
	assert_eq!(round, batch);
	// Defaults: a bare ticket parses (missing fields default).
	let bare: BatchTicketSpec = serde_json::from_str(r#"{"ticket":1,"slot":2}"#).unwrap();
	assert_eq!(bare.ticket, 1);
	assert_eq!(bare.slot, 2);
	assert_eq!(bare.format, 0);
	assert!(bare.montage.is_empty());
	assert!(
		bare.adjustments.is_empty(),
		"v1 tickets carry no adjustment layers"
	);
}

/// Protocol v2 compatibility: a v1-shaped montage clip (no `effects`
/// field) parses with an empty effect stack, and the node-value wire
/// enum round-trips every plain-data variant.
#[test]
fn montage_clip_effects_are_additive_and_values_roundtrip() {
	let legacy = json!({
		"filename": "a.mp4",
		"stream_index": 0,
		"in_num": 0, "in_den": 24,
		"out_num": 48, "out_den": 24,
		"media_in_num": 0, "media_in_den": 24,
		"gain": 1.0,
	});
	let parsed: WireMontageClip = serde_json::from_value(legacy).unwrap();
	assert!(parsed.effects.is_empty(), "v1 clips carry no effects");
	assert!(
		parsed.audio_effects.is_empty(),
		"v1 clips carry no audio effects"
	);
	assert_eq!(parsed.pan, 0.0, "v1 clips are unpanned");
	assert_eq!(parsed.fade_in_den, 0, "v1 clips have no fade fields");
	// The conversion treats the missing 0/0 rationals as "no fade"
	// rather than a NaN duration.
	let clip = montage_clip_from(&parsed);
	assert_eq!(clip.fade_in, quercus_core::Rational::new(0, 1));
	assert_eq!(clip.fade_out, quercus_core::Rational::new(0, 1));
	assert_eq!(clip.pan, 0.0);
	assert!(clip.audio_effects.is_empty());

	// Ticket -> wire -> ticket keeps the audio post fields.
	let mut ticket_clip = clip;
	ticket_clip.gain = 0.5;
	ticket_clip.pan = 0.25;
	ticket_clip.fade_in = quercus_core::Rational::new(1, 8);
	ticket_clip.fade_out = quercus_core::Rational::new(3, 8);
	ticket_clip.audio_effects = vec![crate::ticket::MontageAudioEffect {
		type_id: "org.olivevideoeditor.Olive.pan".into(),
		enabled: false,
		params: vec![(
			"panning_in".into(),
			quercus_node::value::NodeValue::Float(-0.25),
		)],
	}];
	let wire = wire_montage_clip_from(&ticket_clip);
	assert_eq!(wire.pan, 0.25);
	assert_eq!((wire.fade_in_num, wire.fade_in_den), (1, 8));
	assert_eq!((wire.fade_out_num, wire.fade_out_den), (3, 8));
	let back = montage_clip_from(&wire);
	assert_eq!(back.gain, ticket_clip.gain);
	assert_eq!(back.pan, ticket_clip.pan);
	assert_eq!(back.fade_in, ticket_clip.fade_in);
	assert_eq!(back.fade_out, ticket_clip.fade_out);
	assert_eq!(back.audio_effects, ticket_clip.audio_effects);

	use quercus_node::value::NodeValue as NV;
	let cases = [
		NV::None,
		NV::Int(-3),
		NV::Float(0.25),
		NV::Color([0.1, 0.2, 0.3, 1.0]),
		NV::Text("hello".into()),
		NV::Boolean(true),
		NV::Rational(quercus_core::Rational::new(1, 24)),
		NV::Vec2([1.0, 2.0]),
		NV::Vec3([1.0, 2.0, 3.0]),
		NV::Vec4([1.0, 2.0, 3.0, 4.0]),
		NV::Matrix([0.0; 16]),
		NV::Combo(2),
		NV::StrCombo("choice".into()),
		NV::Binary(vec![1, 2, 3]),
	];
	for value in cases {
		let wire = WireNodeValue::from_node_value(&value).expect("plain data has a wire form");
		let json = serde_json::to_string(&wire).unwrap();
		let back: WireNodeValue = serde_json::from_str(&json).unwrap();
		assert_eq!(back.to_node_value(), value, "wire round-trip of {value:?}");
	}
	// Connection/handle variants have no wire form.
	assert!(WireNodeValue::from_node_value(&NV::PushButton).is_none());
}

#[test]
fn batch_accepted_and_frame_failed_wire_roundtrip() {
	let accepted = BatchAcceptedMsg {
		batch_id: 9,
		tickets: vec![1, 2, 3],
	};
	let value = serde_json::to_value(&accepted).unwrap();
	assert_eq!(value["batch_id"], 9);
	assert_eq!(value["tickets"], json!([1, 2, 3]));
	let round: BatchAcceptedMsg = serde_json::from_value(value).unwrap();
	assert_eq!(round, accepted);

	let failed = FrameFailedMsg {
		ticket: 12,
		error: "decode failed".into(),
	};
	let value = serde_json::to_value(&failed).unwrap();
	assert_eq!(value["ticket"], 12);
	assert_eq!(value["error"], "decode failed");
	let round: FrameFailedMsg = serde_json::from_value(value).unwrap();
	assert_eq!(round, failed);
	// Wire line with the type tag parses too.
	let tagged: FrameFailedMsg = serde_json::from_value(json!({
		"type": TYPE_FRAME_FAILED,
		"ticket": 12,
		"error": "decode failed",
	}))
	.unwrap();
	assert_eq!(tagged, failed);
}

#[test]
fn render_audio_batch_wire_roundtrip() {
	// M15 S3: the audio batch message reuses the claim/credit flow of
	// render_batch with a dedicated ticket spec.
	let batch = RenderAudioBatchMsg {
		batch_id: 5,
		tickets: vec![AudioTicketSpec {
			ticket: 41,
			slot: 0,
			time_num: 0,
			time_den: 48000,
			duration_num: 1600,
			duration_den: 48000,
			sample_rate: 48000,
			channel_layout: 0x3,
			channels: 2,
			montage: vec![WireMontageClip {
				filename: "a.mp4".into(),
				stream_index: 0,
				in_num: 0,
				in_den: 24,
				out_num: 48,
				out_den: 24,
				media_in_num: 10,
				media_in_den: 24,
				gain: 0.5,
				pan: 0.0,
				fade_in_num: 0,
				fade_in_den: 24,
				fade_out_num: 0,
				fade_out_den: 24,
				audio_effects: vec![],
				effects: vec![],
			}],
			bus_effects: vec![WireMontageAudioEffect {
				type_id: "org.olivevideoeditor.Olive.limiter".into(),
				enabled: true,
				params: vec![WireEffectParam {
					input: "ceiling_db".into(),
					value: WireNodeValue::Float(-1.0),
				}],
			}],
		}],
	};
	let value = serde_json::to_value(&batch).unwrap();
	assert_eq!(value["batch_id"], 5);
	assert_eq!(value["tickets"][0]["ticket"], 41);
	assert_eq!(value["tickets"][0]["slot"], 0);
	assert_eq!(value["tickets"][0]["sample_rate"], 48000);
	assert_eq!(value["tickets"][0]["channel_layout"], 3);
	assert_eq!(value["tickets"][0]["channels"], 2);
	assert_eq!(value["tickets"][0]["duration_num"], 1600);
	assert_eq!(value["tickets"][0]["montage"][0]["filename"], "a.mp4");
	assert_eq!(
		value["tickets"][0]["bus_effects"][0]["type_id"],
		"org.olivevideoeditor.Olive.limiter"
	);
	let round: RenderAudioBatchMsg = serde_json::from_value(value).unwrap();
	assert_eq!(round, batch);
	// Defaults: a bare audio ticket parses (missing fields default).
	let bare: AudioTicketSpec = serde_json::from_str(r#"{"ticket":1,"slot":2}"#).unwrap();
	assert_eq!(bare.ticket, 1);
	assert_eq!(bare.slot, 2);
	assert_eq!(bare.sample_rate, 0);
	assert!(bare.montage.is_empty());
	assert!(
		bare.bus_effects.is_empty(),
		"v2 additive bus field defaults"
	);
}

#[test]
fn v2_type_constants_are_stable_wire_names() {
	assert_eq!(TYPE_HELLO_CAPS, "hello_caps");
	assert_eq!(TYPE_RENDER_BATCH, "render_batch");
	assert_eq!(TYPE_BATCH_ACCEPTED, "batch_accepted");
	assert_eq!(TYPE_FRAME_FAILED, "frame_failed");
	assert_eq!(TYPE_RENDER_AUDIO_BATCH, "render_audio_batch");
	assert_eq!(TYPE_SHUTDOWN, "shutdown");
	// BGRA8 slot format stays outside the PixelFormat enum range; the
	// audio slot format follows it (M15 S3).
	assert_eq!(SLOT_FORMAT_BGRA8, 100);
	assert_eq!(SLOT_FORMAT_AUDIO_F32, 101);
}

// ---- Shared-memory transport -----------------------------------------

/// A unique, temporary POSIX segment key for a test (pid + counter), so
/// parallel test runs never collide.
fn test_key(name: &str) -> String {
	static COUNTER: AtomicU32 = AtomicU32::new(0);
	let n = COUNTER.fetch_add(1, Ordering::Relaxed);
	SharedMemoryRegion::make_key(i64::from(std::process::id()), (n & 0x7FFF) as i32)
		+ &format!("-{name}")
}

/// Create one segment and map it a second time — the in-process
/// equivalent of two processes sharing a segment. Returns
/// `(owner_region, peer_region)`; both must be kept alive for the
/// whole test (the peer is an attach that does not unlink).
fn two_mappings(key: &str, size: usize) -> (SharedMemoryRegion, SharedMemoryRegion) {
	let mut owner = SharedMemoryRegion::new();
	assert!(
		owner.open(key, size, ShmMode::Create),
		"create failed: {}",
		owner.error()
	);
	let mut peer = SharedMemoryRegion::new();
	assert!(
		peer.open(key, size, ShmMode::Attach),
		"attach failed: {}",
		peer.error()
	);
	(owner, peer)
}

// ---- SpscRingBuffer -------------------------------------------------

#[test]
fn ring_bytes_needed_matches_cpp_layout() {
	// 12 header bytes + capacity * 4.
	assert_eq!(SpscRingBuffer::bytes_needed(4), 12 + 16);
	assert_eq!(SpscRingBuffer::bytes_needed(5), 12 + 20);
	assert_eq!(SpscRingBuffer::bytes_needed(0), 12);
}

#[test]
fn ring_empty_full_and_single_entry() {
	let key = test_key("ring-empty");
	let size = SpscRingBuffer::bytes_needed(4);
	let (owner, peer) = two_mappings(&key, size);
	// SAFETY: both mappings are live and at least `size` bytes.
	let prod = unsafe { SpscRingBuffer::create(owner.data(), 4) };
	let cons = unsafe { SpscRingBuffer::attach(peer.data()) };

	assert!(unsafe { cons.is_empty_approx() });
	let mut v = 99;
	assert!(!unsafe { cons.pop(&mut v) });
	assert_eq!(v, 99);

	assert!(unsafe { prod.push(7) });
	assert!(!unsafe { cons.is_empty_approx() });
	assert_eq!(unsafe { cons.size_approx() }, 1);
	assert!(unsafe { cons.pop(&mut v) });
	assert_eq!(v, 7);
	assert!(unsafe { cons.is_empty_approx() });
}

#[test]
fn ring_capacity_minus_one_live_entries() {
	// A ring of capacity N holds at most N-1 entries (one slot is
	// always left empty to tell full from empty).
	let key = test_key("ring-cap");
	let size = SpscRingBuffer::bytes_needed(4);
	let (owner, peer) = two_mappings(&key, size);
	// SAFETY: live mappings.
	let prod = unsafe { SpscRingBuffer::create(owner.data(), 4) };
	let cons = unsafe { SpscRingBuffer::attach(peer.data()) };

	for i in 0..3 {
		assert!(unsafe { prod.push(i) });
	}
	// The 4th push must fail: head would collide with tail.
	assert!(!unsafe { prod.push(99) });

	let mut v = 0;
	for expected in 0..3 {
		assert!(unsafe { cons.pop(&mut v) });
		assert_eq!(v, expected);
	}
	assert!(!unsafe { cons.pop(&mut v) });
}

#[test]
fn ring_wraparound_preserves_order() {
	// Fill, drain, then wrap past the end of the slot array: cursors
	// are modulo-capacity, order must be preserved across the wrap.
	let key = test_key("ring-wrap");
	let size = SpscRingBuffer::bytes_needed(4);
	let (owner, peer) = two_mappings(&key, size);
	// SAFETY: live mappings.
	let prod = unsafe { SpscRingBuffer::create(owner.data(), 4) };
	let cons = unsafe { SpscRingBuffer::attach(peer.data()) };

	for i in 0..3 {
		assert!(unsafe { prod.push(i) });
	}
	let mut v = 0;
	for _ in 0..3 {
		assert!(unsafe { cons.pop(&mut v) });
	}
	// Ring is empty again; push past the wrap point.
	for i in 3..6 {
		assert!(unsafe { prod.push(i) });
	}
	for expected in 3..6 {
		assert!(unsafe { cons.pop(&mut v) });
		assert_eq!(v, expected);
	}
}

// ---- FrameSlotPool --------------------------------------------------

#[test]
fn framepool_bytes_needed_matches_cpp_offsets() {
	// Recompute by hand with the C++ layout: header 64, each ring
	// align_up(12 + 4*(n+1), 64), meta align_up(176*n, 64), data
	// align_up(slot_bytes, 64) * n.
	let check = |n: u32, slot: usize| {
		let ring = align_up(12 + 4 * (n as usize + 1), 64);
		let expected =
			64 + ring + ring + align_up(176 * n as usize, 64) + align_up(slot, 64) * n as usize;
		assert_eq!(FrameSlotPool::bytes_needed(n, slot), expected);
	};
	check(4, 4096);
	check(6, 1_000_000);
	check(1, 64);
	check(3, 100);
}

#[test]
fn framepool_create_attach_two_processes_both_directions() {
	// "Two processes": two mappings of the same segment. Owner creates
	// the pool; the peer attaches. A filler on one side and a drainer
	// on the other exchange slots in both directions.
	let key = test_key("pool-bidi");
	let slots = 4u32;
	let slot_bytes = 64usize;
	let size = FrameSlotPool::bytes_needed(slots, slot_bytes);
	let (owner, peer) = two_mappings(&key, size);

	// SAFETY: both mappings are live and sized by bytes_needed.
	let filler = unsafe { FrameSlotPool::create(owner.data(), slots, slot_bytes) };
	let drainer = unsafe { FrameSlotPool::attach(peer.data()) };

	assert!(filler.is_valid());
	assert!(drainer.is_valid());
	assert_eq!(drainer.slot_count(), slots);
	assert_eq!(drainer.slot_data_bytes(), slot_bytes);

	// Filler acquires every slot exactly once (seeded free ring), then
	// the free ring is empty.
	let mut got = Vec::new();
	for _ in 0..slots {
		let mut s = 0;
		assert!(unsafe { filler.acquire(&mut s) });
		got.push(s);
	}
	got.sort_unstable();
	assert_eq!(got, vec![0, 1, 2, 3]);
	let mut extra = 0;
	assert!(!unsafe { filler.acquire(&mut extra) });
	// Drainer sees nothing ready yet.
	assert!(!unsafe { drainer.consume(&mut extra) });

	// Filler writes pixels + meta into two slots and publishes them.
	for (i, slot) in [0u32, 2u32].iter().enumerate() {
		// SAFETY: `slot` was acquired above.
		let data = unsafe { filler.slot_data(*slot) };
		unsafe { ptr::write_bytes(data, (i * 40 + 1) as u8, slot_bytes) };
		// SAFETY: slot in range.
		let meta = unsafe { &mut *filler.meta(*slot) };
		meta.id = 100 + *slot as i64;
		meta.width = 8;
		meta.height = 8;
		meta.data_size = slot_bytes as i32;
		assert!(unsafe { filler.publish(*slot) });
	}

	// Drainer consumes them through its own mapping and sees the same
	// payloads and metadata.
	let mut consumed = Vec::new();
	for _ in 0..2 {
		let mut s = 0;
		assert!(unsafe { drainer.consume(&mut s) });
		// SAFETY: s was consumed.
		let data = unsafe { drainer.slot_data_const(s) };
		let meta = unsafe { &*drainer.meta_const(s) };
		assert_eq!(meta.id, 100 + s as i64);
		assert_eq!(meta.width, 8);
		assert_eq!(meta.data_size, slot_bytes as i32);
		// SAFETY: slot_bytes readable in the slot block.
		let first = unsafe { *data };
		assert_eq!(first, ((s as usize / 2) * 40 + 1) as u8);
		consumed.push(s);
	}
	consumed.sort_unstable();
	assert_eq!(consumed, vec![0, 2]);
	assert!(!unsafe { drainer.consume(&mut extra) });

	// Drainer releases the slots back; the filler can acquire them
	// again — the full round trip through both rings.
	for s in consumed {
		assert!(unsafe { drainer.release(s) });
	}
	let mut s = 0;
	assert!(unsafe { filler.acquire(&mut s) });
	assert_eq!(s, 0);
}

#[test]
fn framepool_wraparound_and_full_edges() {
	// Small pool: cycle every slot many times, verifying the rings'
	// modulo behavior end to end.
	let key = test_key("pool-wrap");
	let slots = 3u32;
	let slot_bytes = 32usize;
	let size = FrameSlotPool::bytes_needed(slots, slot_bytes);
	let (owner, peer) = two_mappings(&key, size);

	// SAFETY: live mappings.
	let filler = unsafe { FrameSlotPool::create(owner.data(), slots, slot_bytes) };
	let drainer = unsafe { FrameSlotPool::attach(peer.data()) };

	for cycle in 0..4u32 {
		let mut published = Vec::new();
		for _ in 0..slots {
			let mut s = 0;
			assert!(unsafe { filler.acquire(&mut s) }, "cycle {cycle}");
			// SAFETY: acquired slot.
			unsafe { ptr::write_bytes(filler.slot_data(s), cycle as u8, slot_bytes) };
			// SAFETY: slot in range.
			let meta = unsafe { &mut *filler.meta(s) };
			meta.id = i64::from(cycle * 100 + s);
			assert!(unsafe { filler.publish(s) });
			published.push(s);
		}
		// Pool is full on the filler side.
		let mut x = 0;
		assert!(!unsafe { filler.acquire(&mut x) });

		// Drain everything on the drainer side.
		let mut consumed = Vec::new();
		for _ in 0..slots {
			let mut s = 0;
			assert!(unsafe { drainer.consume(&mut s) });
			// SAFETY: consumed slot.
			let meta = unsafe { &*drainer.meta_const(s) };
			assert_eq!(meta.id, i64::from(cycle * 100 + s));
			// SAFETY: 1 byte readable.
			assert_eq!(unsafe { *drainer.slot_data_const(s) }, cycle as u8);
			consumed.push(s);
		}
		assert!(!unsafe { drainer.consume(&mut x) });
		consumed.sort_unstable();
		assert_eq!(consumed, vec![0, 1, 2]);

		for s in consumed {
			assert!(unsafe { drainer.release(s) });
		}
	}
}

#[test]
fn framepool_attach_rejects_wrong_magic() {
	let key = test_key("pool-badmagic");
	let size = FrameSlotPool::bytes_needed(2, 16);
	let (owner, _peer) = two_mappings(&key, size);
	// Overwrite the header area with garbage — no pool magic.
	// SAFETY: owner mapping is live.
	unsafe { ptr::write_bytes(owner.data(), 0xAB, 64) };
	// SAFETY: buffer is live.
	let pool = unsafe { FrameSlotPool::attach(owner.data()) };
	assert!(!pool.is_valid());
	assert_eq!(pool.slot_count(), 0);
	assert_eq!(pool.slot_data_bytes(), 0);
}

#[test]
fn framepool_pool_over_reused_segment_is_consistent() {
	// A pool that has been cycled fully and then attached fresh reports
	// the same geometry as bytes_needed computed it.
	let key = test_key("pool-geometry");
	let slots = 5u32;
	let slot_bytes = 1000usize;
	let size = FrameSlotPool::bytes_needed(slots, slot_bytes);
	let (owner, peer) = two_mappings(&key, size);
	// SAFETY: live mappings.
	let _ = unsafe { FrameSlotPool::create(owner.data(), slots, slot_bytes) };
	let attached = unsafe { FrameSlotPool::attach(peer.data()) };
	assert!(attached.is_valid());
	assert_eq!(attached.slot_count(), slots);
	assert_eq!(attached.slot_data_bytes(), slot_bytes);
	// Slot stride is 64-aligned (matches the C++ data layout).
	// SAFETY: valid pool.
	let s0 = unsafe { attached.slot_data(0) };
	let s1 = unsafe { attached.slot_data(1) };
	assert_eq!(s1 as usize - s0 as usize, align_up(slot_bytes, K_ALIGN));
}

// ---- SharedMemoryRegion ---------------------------------------------

#[test]
fn region_create_attach_write_visibility() {
	let key = test_key("region-vis");
	let size = 4096usize;
	let (mut owner, mut peer) = two_mappings(&key, size);
	assert!(owner.is_valid());
	assert!(peer.is_valid());
	assert_eq!(owner.size(), size);
	assert_eq!(peer.size(), size);
	assert_eq!(owner.key(), key);
	assert_eq!(peer.key(), key);

	// Owner writes; peer sees it through its own mapping.
	// SAFETY: both mappings are live with `size` bytes.
	unsafe {
		let dst = owner.data() as *mut u32;
		*dst = 0xDEADBEEF;
	}
	// SAFETY: peer mapping live.
	let seen = unsafe { *(peer.data() as *const u32) };
	assert_eq!(seen, 0xDEADBEEF);

	// Peer writes back; owner sees it.
	// SAFETY: peer mapping live.
	unsafe {
		let dst = peer.data() as *mut u32;
		*dst = 0x12345678;
	}
	// SAFETY: owner mapping live.
	assert_eq!(unsafe { *(owner.data() as *const u32) }, 0x12345678);

	// Closing the ATTACH side does not unlink: while the owner lives,
	// a third mapping can still open the name.
	peer.close();
	assert!(!peer.is_valid());
	let mut third = SharedMemoryRegion::new();
	assert!(third.open(&key, size, ShmMode::Attach), "{}", third.error());
	assert!(third.is_valid());
	third.close();

	// Closing the OWNER unlinks the segment; further attaches fail.
	owner.close();
	assert!(!owner.is_valid());
	let mut fourth = SharedMemoryRegion::new();
	assert!(!fourth.open(&key, size, ShmMode::Attach));
}

#[test]
#[cfg(windows)]
fn region_windows_create_attach_roundtrip() {
	// The Windows backend round trip — create -> write -> open -> read —
	// with the same assertions as the platform-neutral
	// `region_create_attach_write_visibility` test above.
	let key = test_key("win-roundtrip");
	let size = 4096usize;
	let (mut owner, mut peer) = two_mappings(&key, size);
	assert!(owner.is_valid());
	assert!(peer.is_valid());
	assert_eq!(owner.size(), size);
	assert_eq!(peer.size(), size);
	assert_eq!(owner.key(), key);
	assert_eq!(peer.key(), key);

	// Owner writes; peer sees it through its own mapping, and vice
	// versa.
	// SAFETY: both mappings are live with `size` bytes.
	unsafe {
		let dst = owner.data() as *mut u32;
		*dst = 0xDEADBEEF;
	}
	// SAFETY: peer mapping live.
	assert_eq!(unsafe { *(peer.data() as *const u32) }, 0xDEADBEEF);
	// SAFETY: peer mapping live.
	unsafe {
		let dst = peer.data() as *mut u32;
		*dst = 0x12345678;
	}
	// SAFETY: owner mapping live.
	assert_eq!(unsafe { *(owner.data() as *const u32) }, 0x12345678);

	// The object dies when the LAST handle closes: closing the attach
	// side leaves the owner's object open (a third attach still
	// works); closing the owner destroys it (further attaches fail).
	peer.close();
	assert!(!peer.is_valid());
	let mut third = SharedMemoryRegion::new();
	assert!(third.open(&key, size, ShmMode::Attach), "{}", third.error());
	assert!(third.is_valid());
	third.close();

	owner.close();
	assert!(!owner.is_valid());
	let mut fourth = SharedMemoryRegion::new();
	assert!(!fourth.open(&key, size, ShmMode::Attach));
}

#[test]
#[cfg(unix)]
fn region_create_replaces_stale_segment() {
	// Mirrors the C++: Create unlinks any stale segment with the same
	// name first (crash cleanup), so a second Create SUCCEEDS and owns
	// a fresh, zeroed segment.
	let key = test_key("region-exists");
	let size = 128usize;
	let (owner, _peer) = two_mappings(&key, size);
	assert!(owner.is_valid());
	// SAFETY: owner mapping live.
	unsafe { *(owner.data() as *mut u32) = 0xCAFEBABE };

	let mut second = SharedMemoryRegion::new();
	assert!(
		second.open(&key, size, ShmMode::Create),
		"{}",
		second.error()
	);
	assert!(second.is_valid());
	// The replacement segment is fresh (zeroed by create).
	// SAFETY: second mapping live.
	assert_eq!(unsafe { *(second.data() as *const u32) }, 0);
}

#[test]
#[cfg(windows)]
fn region_create_fails_on_live_existing_segment() {
	// Windows has no way to unlink a live section object (it dies only
	// when its last handle closes), so a second Create while the owner
	// still holds the object fails like O_EXCL — the reverse of the
	// POSIX stale-replace behavior above, which unlinks first.
	let key = test_key("region-exists");
	let size = 128usize;
	let (owner, _peer) = two_mappings(&key, size);
	assert!(owner.is_valid());
	// SAFETY: owner mapping live.
	unsafe { *(owner.data() as *mut u32) = 0xCAFEBABE };

	let mut second = SharedMemoryRegion::new();
	assert!(!second.open(&key, size, ShmMode::Create));
	assert!(!second.is_valid());
	assert!(!second.error().is_empty());
}

#[test]
fn region_attach_fails_when_segment_too_small() {
	// macOS rounds shm segment sizes up to a 16 KiB minimum, so use
	// sizes above that to exercise the size check.
	let key = test_key("region-small");
	let (owner, _peer) = two_mappings(&key, 4096);
	assert!(owner.is_valid());

	// Attaching with a larger size than the segment must fail (the
	// fstat check, mirroring the C++).
	let mut big = SharedMemoryRegion::new();
	assert!(!big.open(&key, 65536, ShmMode::Attach));
	assert!(!big.is_valid());
	assert!(!big.error().is_empty());
}

#[test]
fn region_make_key_format() {
	assert_eq!(SharedMemoryRegion::make_key(4242, 3), "olive-rw-4242-3");
	assert_eq!(SharedMemoryRegion::make_key(1, 0), "olive-rw-1-0");
}

#[cfg(unix)]
#[test]
fn region_shm_name_folds_long_keys_for_macos() {
	// macOS rejects POSIX shm names longer than PSHMNAMLEN (31 bytes
	// including the leading slash) with ENAMETOOLONG; longer keys must
	// fold to a deterministic short name so an owner and a worker
	// still meet on the same segment.
	let long = "quercus-procpool-ut-4242-audio-release";
	let name = SharedMemoryRegion::shm_name_for_key(long);
	assert!(name.len() <= 31, "folded name too long: {name}");
	assert!(name.starts_with('/') && !name[1..].contains('/'), "{name}");
	assert_eq!(
		name,
		SharedMemoryRegion::shm_name_for_key(long),
		"the fold is deterministic"
	);
	assert_ne!(
		name,
		SharedMemoryRegion::shm_name_for_key("quercus-procpool-ut-4242-audio-other"),
		"different keys keep different names"
	);
	// Short keys keep their exact spelling.
	assert_eq!(
		SharedMemoryRegion::shm_name_for_key("olive-rw-4242-0"),
		"/olive-rw-4242-0"
	);
	assert_eq!(
		SharedMemoryRegion::shm_name_for_key("a/b/c"),
		"/a_b_c",
		"slashes become underscores before folding"
	);
}

#[test]
fn region_shm_spike_512mb_segment() {
	// M15 S1 shm spike: one POSIX shm segment must ftruncate to at
	// least 512 MiB (the designed per-worker pool is 8 slots x ~8.3 MB
	// BGRA8 1080p ≈ 66 MB, but large F32 4K pools reach the hundreds
	// of MB). Verified on macOS: POSIX shm is NOT bounded by the SysV
	// shmmax sysctl; segments up to 1 GiB spiked fine, so no
	// temp-file+mmap fallback backend is needed. This test is the
	// regression gate; a failure here means the platform needs the
	// fallback backend inside `SharedMemoryRegion::open`.
	let key = test_key("spike-512mb");
	let size = 512usize * 1024 * 1024;
	let mut owner = SharedMemoryRegion::new();
	assert!(
		owner.open(&key, size, ShmMode::Create),
		"512 MiB shm segment must be creatable: {}",
		owner.error()
	);
	assert!(owner.is_valid());
	assert_eq!(owner.size(), size);
	// Touch the first and last byte of the mapping: an overcommitted
	// segment would SIGBUS here.
	// SAFETY: owner mapping is live with `size` bytes.
	unsafe {
		*owner.data() = 0xA5;
		*owner.data().add(size - 1) = 0x5A;
	}
	let mut peer = SharedMemoryRegion::new();
	assert!(
		peer.open(&key, size, ShmMode::Attach),
		"512 MiB shm segment must be attachable: {}",
		peer.error()
	);
	// SAFETY: peer mapping is live with `size` bytes.
	unsafe {
		assert_eq!(*peer.data(), 0xA5);
		assert_eq!(*peer.data().add(size - 1), 0x5A);
	}
	peer.close();
	owner.close();
}

#[test]
fn region_keys_are_isolation_safe() {
	// Keys with slashes are flattened to a single-slash POSIX name.
	let key = "a/b/c";
	let size = 64usize;
	let (owner, peer) = two_mappings(key, size);
	assert!(owner.is_valid());
	assert!(peer.is_valid());
	// The actual POSIX name is "/a_b_c".
	// SAFETY: mapping live.
	unsafe { *(owner.data() as *mut u32) = 7 };
	// SAFETY: peer mapping live.
	assert_eq!(unsafe { *(peer.data() as *const u32) }, 7);
}
