use super::*;
use crate::error::Error;
use crate::handle::CHandle;
use crate::input::Input;
use crate::node::{NodeBehavior, NodeCore};
use crate::nodes::graphendpoints::{GRAPH_INPUT_FEED_INPUT, GRAPH_OUTPUT_INPUT};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// What a [`TestNode`] pushes to its own output table.
enum Emit {
	/// Nothing at all.
	Nothing,
	/// A constant float.
	Float(f64),
	/// The time the node was evaluated at.
	Time,
	/// A null texture handle.
	Texture,
}

/// Counting stand-in for a real node: emits on demand so a test can
/// observe evaluation counts, dequeue order, and input rows.
struct TestNode {
	emit: Emit,
	calls: Arc<AtomicUsize>,
}

impl NodeBehavior for TestNode {
	fn name(&self) -> &str {
		"Test Node"
	}

	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.test-bfs-node"
	}

	fn value(
		&self,
		_core: &NodeCore,
		_inputs: &NodeValueRow,
		time: Rational,
		table: &mut NodeValueTable,
	) {
		self.calls.fetch_add(1, Ordering::Relaxed);
		match self.emit {
			Emit::Nothing => {}
			Emit::Float(value) => table.push(ValueType::Float, NodeValue::Float(value), None),
			Emit::Time => table.push(ValueType::Float, NodeValue::Float(time.to_f64()), None),
			Emit::Texture => table.push(
				ValueType::Texture,
				NodeValue::Texture(CHandle::null()),
				None,
			),
		}
	}

	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		None
	}
}

/// Add a [`TestNode`] declaring `inputs`; returns its id and call counter.
fn add_node(
	graph: &mut Graph,
	inputs: &[(&str, ValueType)],
	emit: Emit,
) -> (NodeId, Arc<AtomicUsize>) {
	let calls = Arc::new(AtomicUsize::new(0));
	let mut core = NodeCore::new();
	for (id, value_type) in inputs {
		core.add_input(Input::new(id, *value_type, NodeValue::None));
	}
	let id = graph.add_node(
		core,
		Box::new(TestNode {
			emit,
			calls: Arc::clone(&calls),
		}),
	);
	(id, calls)
}

/// The endpoint pair with the default `input -> output` edge removed,
/// so a test can wire the convergence itself.
fn detached_endpoints(graph: &mut Graph) -> (NodeId, NodeId) {
	let (input, output) = graph.ensure_endpoints();
	graph.disconnect(input, output, GRAPH_OUTPUT_INPUT, -1);
	(input, output)
}

/// A node that samples `val_in` one second late: observes that the
/// sweep pulls a time-shifted upstream through the DFS memo.
struct Delay {
	rows: Arc<Mutex<Vec<NodeValueRow>>>,
}

impl NodeBehavior for Delay {
	fn name(&self) -> &str {
		"Delay"
	}

	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.test-bfs-delay"
	}

	fn input_time_adjustment(
		&self,
		_core: &NodeCore,
		input: &str,
		_element: i32,
		time: TimeRange,
		_traverse: bool,
	) -> TimeRange {
		if input == "val_in" {
			TimeRange::new(
				time.in_() + Rational::new(1, 1),
				time.out() + Rational::new(1, 1),
			)
		} else {
			time
		}
	}

	fn value(
		&self,
		_core: &NodeCore,
		inputs: &NodeValueRow,
		_time: Rational,
		table: &mut NodeValueTable,
	) {
		self.rows.lock().expect("delay rows").push(inputs.clone());
		if let Some(value @ NodeValue::Float(_)) = inputs.get("val_in") {
			table.push(ValueType::Float, value.clone(), None);
		}
	}

	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		None
	}
}

/// Records resolve order/rows and can replace a node's resolved table.
struct Probe {
	order: Vec<NodeId>,
	rows: HashMap<NodeId, NodeValueRow>,
	replace: Option<(NodeId, Vec<(ValueType, NodeValue)>)>,
}

impl Probe {
	fn new() -> Probe {
		Probe {
			order: Vec::new(),
			rows: HashMap::new(),
			replace: None,
		}
	}
}

impl RenderHooks for Probe {
	fn resolve(&mut self, node: NodeId, row: &NodeValueRow, table: &mut NodeValueTable) {
		self.order.push(node);
		self.rows.insert(node, row.clone());
		if let Some((target, rows)) = &self.replace {
			if *target == node {
				table.clear();
				for (ty, value) in rows {
					table.push(*ty, value.clone(), None);
				}
			}
		}
	}
}

#[test]
fn bfs_starts_at_graph_input_and_skips_unreachable_nodes() {
	let mut graph = Graph::new();
	let (input, output) = detached_endpoints(&mut graph);
	let (dead, dead_calls) = add_node(&mut graph, &[("x_in", ValueType::Texture)], Emit::Nothing);
	let (_island, island_calls) = add_node(&mut graph, &[], Emit::Float(1.0));
	graph
		.connect(input, dead, "x_in", -1)
		.expect("input -> dead");
	graph
		.connect(input, output, GRAPH_OUTPUT_INPUT, -1)
		.expect("input -> output");

	let mut probe = Probe::new();
	let value = Traverser::new()
		.eval_graph_bfs(&graph, Rational::new(0, 1), &mut probe)
		.expect("the input -> output edge alone is a valid graph");
	assert_eq!(probe.order, vec![input, output]);
	assert_eq!(dead_calls.load(Ordering::Relaxed), 0, "dead branch skipped");
	assert_eq!(
		island_calls.load(Ordering::Relaxed),
		0,
		"island never queued"
	);
	assert_eq!(value, NodeValue::None);
}

#[test]
fn bfs_waits_for_every_input_of_a_converging_node() {
	let mut graph = Graph::new();
	let (input, output) = detached_endpoints(&mut graph);
	let (a, _) = add_node(
		&mut graph,
		&[("tex_in", ValueType::Texture)],
		Emit::Float(1.0),
	);
	let (b, _) = add_node(
		&mut graph,
		&[("tex_in", ValueType::Texture)],
		Emit::Float(2.0),
	);
	let (merge, _) = add_node(
		&mut graph,
		&[("a_in", ValueType::Float), ("b_in", ValueType::Float)],
		Emit::Float(3.0),
	);
	graph.connect(input, a, "tex_in", -1).expect("input -> a");
	graph.connect(input, b, "tex_in", -1).expect("input -> b");
	graph.connect(a, merge, "a_in", -1).expect("a -> merge");
	graph.connect(b, merge, "b_in", -1).expect("b -> merge");
	graph
		.connect(merge, output, GRAPH_OUTPUT_INPUT, -1)
		.expect("merge -> output");

	let mut probe = Probe::new();
	Traverser::new()
		.eval_graph_bfs(&graph, Rational::new(0, 1), &mut probe)
		.expect("the converging graph is orderable");
	assert_eq!(probe.order, vec![input, a, b, merge, output]);
	let row = probe.rows.get(&merge).expect("merge resolved");
	assert_eq!(row.get("a_in"), Some(&NodeValue::Float(1.0)));
	assert_eq!(row.get("b_in"), Some(&NodeValue::Float(2.0)));
}

#[test]
fn bfs_evaluates_a_shared_source_once() {
	let mut graph = Graph::new();
	let (input, output) = detached_endpoints(&mut graph);
	let (source, source_calls) = add_node(
		&mut graph,
		&[("tex_in", ValueType::Texture)],
		Emit::Float(7.0),
	);
	let (m, _) = add_node(&mut graph, &[("a_in", ValueType::Float)], Emit::Float(1.0));
	let (n, _) = add_node(&mut graph, &[("a_in", ValueType::Float)], Emit::Float(2.0));
	let (f, _) = add_node(
		&mut graph,
		&[("c_in", ValueType::Float), ("d_in", ValueType::Float)],
		Emit::Float(3.0),
	);
	graph
		.connect(input, source, "tex_in", -1)
		.expect("input -> source");
	graph.connect(source, m, "a_in", -1).expect("source -> m");
	graph.connect(source, n, "a_in", -1).expect("source -> n");
	graph.connect(m, f, "c_in", -1).expect("m -> f");
	graph.connect(n, f, "d_in", -1).expect("n -> f");
	graph
		.connect(f, output, GRAPH_OUTPUT_INPUT, -1)
		.expect("f -> output");

	let mut probe = Probe::new();
	Traverser::new()
		.eval_graph_bfs(&graph, Rational::new(0, 1), &mut probe)
		.expect("the fan-out graph is orderable");
	assert_eq!(probe.order, vec![input, source, m, n, f, output]);
	assert_eq!(
		source_calls.load(Ordering::Relaxed),
		1,
		"the shared source runs once"
	);
	assert_eq!(
		probe.rows.get(&m).and_then(|r| r.get("a_in")),
		Some(&NodeValue::Float(7.0))
	);
	assert_eq!(
		probe.rows.get(&n).and_then(|r| r.get("a_in")),
		Some(&NodeValue::Float(7.0))
	);
}

#[test]
fn bfs_dequeue_order_is_deterministic() {
	let mut graph = Graph::new();
	let (input, output) = detached_endpoints(&mut graph);
	let (a, _) = add_node(
		&mut graph,
		&[("tex_in", ValueType::Texture)],
		Emit::Float(1.0),
	);
	let (b, _) = add_node(
		&mut graph,
		&[("tex_in", ValueType::Texture)],
		Emit::Float(2.0),
	);
	let (c, _) = add_node(
		&mut graph,
		&[("tex_in", ValueType::Texture)],
		Emit::Float(3.0),
	);
	let (merge, _) = add_node(
		&mut graph,
		&[
			("a_in", ValueType::Float),
			("b_in", ValueType::Float),
			("c_in", ValueType::Float),
		],
		Emit::Float(4.0),
	);
	for (from, to, input) in [
		(input, a, "tex_in"),
		(input, b, "tex_in"),
		(input, c, "tex_in"),
		(a, merge, "a_in"),
		(b, merge, "b_in"),
		(c, merge, "c_in"),
		(merge, output, GRAPH_OUTPUT_INPUT),
	] {
		graph
			.connect(from, to, input, -1)
			.unwrap_or_else(|e| panic!("{from:?} -> {to:?}.{input}: {e:?}"));
	}

	let mut first = Probe::new();
	Traverser::new()
		.eval_graph_bfs(&graph, Rational::new(0, 1), &mut first)
		.expect("the fan-in graph is orderable");
	let mut second = Probe::new();
	Traverser::new()
		.eval_graph_bfs(&graph, Rational::new(0, 1), &mut second)
		.expect("the fan-in graph is orderable");
	assert_eq!(first.order, vec![input, a, b, c, merge, output]);
	assert_eq!(first.order, second.order, "same graph -> same order");
}

#[test]
fn bfs_pulls_time_shifted_upstreams_through_the_dfs_memo() {
	let mut graph = Graph::new();
	let (input, output) = detached_endpoints(&mut graph);
	let (source, source_calls) = add_node(&mut graph, &[], Emit::Time);
	let rows = Arc::new(Mutex::new(Vec::new()));
	let mut core = NodeCore::new();
	core.add_input(Input::new("tex_in", ValueType::Texture, NodeValue::None));
	core.add_input(Input::new("val_in", ValueType::Float, NodeValue::None));
	let delay = graph.add_node(
		core,
		Box::new(Delay {
			rows: Arc::clone(&rows),
		}),
	);
	graph
		.connect(input, delay, "tex_in", -1)
		.expect("input -> delay.tex_in");
	graph
		.connect(source, delay, "val_in", -1)
		.expect("source -> delay.val_in");
	graph
		.connect(delay, output, GRAPH_OUTPUT_INPUT, -1)
		.expect("delay -> output");

	let mut probe = Probe::new();
	let value = Traverser::new()
		.eval_graph_bfs(&graph, Rational::new(0, 1), &mut probe)
		.expect("an out-of-live-set feeder does not stall the sweep");
	assert_eq!(
		probe.order,
		vec![input, source, delay, output],
		"the far end is pulled during delay's turn, not queued"
	);
	assert_eq!(source_calls.load(Ordering::Relaxed), 1);
	let rows = rows.lock().expect("delay rows");
	assert_eq!(rows.len(), 1);
	// One second late: the sweep pulled the source at time 1, not 0.
	assert_eq!(rows[0].get("val_in"), Some(&NodeValue::Float(1.0)));
	assert_eq!(value, NodeValue::None);
}

#[test]
fn bfs_hands_resolved_tables_to_downstream_nodes() {
	let mut graph = Graph::new();
	let (input, output) = detached_endpoints(&mut graph);
	let (a, _) = add_node(
		&mut graph,
		&[("tex_in", ValueType::Texture)],
		Emit::Float(1.0),
	);
	let (b, _) = add_node(
		&mut graph,
		&[("tex_in", ValueType::Texture)],
		Emit::Float(2.0),
	);
	let (merge, _) = add_node(
		&mut graph,
		&[("a_in", ValueType::Float), ("b_in", ValueType::Float)],
		Emit::Float(3.0),
	);
	graph.connect(input, a, "tex_in", -1).expect("input -> a");
	graph.connect(input, b, "tex_in", -1).expect("input -> b");
	graph.connect(a, merge, "a_in", -1).expect("a -> merge");
	graph.connect(b, merge, "b_in", -1).expect("b -> merge");
	graph
		.connect(merge, output, GRAPH_OUTPUT_INPUT, -1)
		.expect("merge -> output");

	let mut probe = Probe::new();
	probe.replace = Some((a, vec![(ValueType::Float, NodeValue::Float(999.0))]));
	Traverser::new()
		.eval_graph_bfs(&graph, Rational::new(0, 1), &mut probe)
		.expect("the converging graph is orderable");
	let row = probe.rows.get(&merge).expect("merge resolved");
	assert_eq!(
		row.get("a_in"),
		Some(&NodeValue::Float(999.0)),
		"the downstream row carries what resolve() left in a's table"
	);
	assert_eq!(row.get("b_in"), Some(&NodeValue::Float(2.0)));
}

#[test]
fn bfs_reports_a_cycle_and_names_its_nodes() {
	let mut graph = Graph::new();
	let (input, output) = detached_endpoints(&mut graph);
	let (a, a_calls) = add_node(&mut graph, &[("in", ValueType::Float)], Emit::Float(1.0));
	let (b, b_calls) = add_node(&mut graph, &[("in", ValueType::Float)], Emit::Float(2.0));
	graph.connect(input, a, "in", -1).expect("input -> a");
	graph.connect(a, b, "in", -1).expect("a -> b");
	graph
		.connect(b, output, GRAPH_OUTPUT_INPUT, -1)
		.expect("b -> output");
	graph.force_connect(b, a, "in", -1);

	let mut probe = Probe::new();
	let error = Traverser::new()
		.eval_graph_bfs(&graph, Rational::new(0, 1), &mut probe)
		.expect_err("a cycle must be reported, not ordered");
	let message = match error {
		Error::Failed(message) => message,
		other => panic!("Failed expected, got {other:?}"),
	};
	assert!(
		message.contains("cycle"),
		"message names the failure: {message}"
	);
	assert!(
		message.contains(&format!("{a:?}")),
		"message names {a:?}: {message}"
	);
	assert!(
		message.contains(&format!("{b:?}")),
		"message names {b:?}: {message}"
	);
	assert_eq!(probe.order, vec![input], "nothing past the cycle resolves");
	assert_eq!(a_calls.load(Ordering::Relaxed), 0);
	assert_eq!(b_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn bfs_returns_the_output_texture() {
	let mut graph = Graph::new();
	let (input, output) = detached_endpoints(&mut graph);
	let (source, source_calls) = add_node(&mut graph, &[], Emit::Texture);
	graph
		.connect(source, input, GRAPH_INPUT_FEED_INPUT, -1)
		.expect("source -> feed_in");
	graph
		.connect(input, output, GRAPH_OUTPUT_INPUT, -1)
		.expect("input -> output");

	let mut probe = Probe::new();
	let value = Traverser::new()
		.eval_graph_bfs(&graph, Rational::new(0, 1), &mut probe)
		.expect("a fed input -> output graph is orderable");
	match value {
		NodeValue::Texture(handle) => assert!(handle.ctx.is_null()),
		other => panic!("texture expected, got {other:?}"),
	}
	assert_eq!(probe.order, vec![source, input, output]);
	assert_eq!(source_calls.load(Ordering::Relaxed), 1);
}

#[test]
fn bfs_requires_both_endpoints() {
	let mut probe = Probe::new();
	let error = Traverser::new()
		.eval_graph_bfs(&Graph::new(), Rational::new(0, 1), &mut probe)
		.expect_err("an endpointless graph cannot be swept");
	assert_eq!(error, Error::NotFound);
}
