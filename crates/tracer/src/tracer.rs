use gst::{glib, subclass::prelude::*};

glib::wrapper! {
    pub struct RerunTracer(ObjectSubclass<imp::RerunTracer>)
       @extends gst::Tracer, gst::Object;
}

impl RerunTracer {
    /// Set a shared RecordingStream to be used by this tracer
    pub fn set_recorder(&self, recorder: rerun::RecordingStream) {
        let imp = self.imp();
        let mut state_guard = imp.state.write().unwrap();
        if let Some(state) = state_guard.as_mut() {
            gst::info!(imp::CAT, imp = imp, "Setting external recorder");
            state.set_recorder(recorder);
        } else {
            gst::error!(
                imp::CAT,
                imp = imp,
                "Cannot set recorder: TracingState not initialized"
            );
        }
    }
}

mod imp {
    use crate::pipeline_graph::analyze_pipeline;
    use gst::{glib, prelude::*, subclass::prelude::*};
    use once_cell::sync::Lazy;
    use std::collections::{HashMap, HashSet};
    use std::str::FromStr;
    use std::sync::RwLock;
    use std::time::{Duration, Instant};

    pub(super) static CAT: Lazy<gst::DebugCategory> = Lazy::new(|| {
        gst::DebugCategory::new(
            "reruntracing",
            gst::DebugColorFlags::all(),
            Some("Rerun pipeline tracer"),
        )
    });

    #[derive(Debug)]
    pub struct TracingState {
        last_logged: HashMap<gst::Pipeline, Instant>,
        pending_logs: HashSet<gst::Pipeline>,
        timeout_source_id: Option<glib::SourceId>,
        debounce_ms: u64,
        rec: Option<rerun::RecordingStream>,
        entity_path: String,
    }

    impl TracingState {
        fn new(debounce_ms: u64, rec: Option<rerun::RecordingStream>, entity_path: String) -> Self {
            Self {
                debounce_ms,
                rec,
                entity_path,
                last_logged: HashMap::new(),
                pending_logs: HashSet::new(),
                timeout_source_id: None,
            }
        }

        pub(super) fn set_recorder(&mut self, recorder: rerun::RecordingStream) {
            self.rec = Some(recorder);
        }

        fn schedule_deferred_log(
            &mut self,
            pipeline: &gst::Pipeline,
            rec: rerun::RecordingStream,
            entity_path: String,
        ) {
            self.pending_logs.insert(pipeline.clone());

            if let Some(source_id) = self.timeout_source_id.take() {
                source_id.remove();
            }

            let pending_pipeline = pipeline.clone();
            let debounce_duration = Duration::from_millis(self.debounce_ms);
            let source_id = glib::timeout_add(debounce_duration, move || {
                // Analyze the pipeline and log each bin
                let Ok(tree) = analyze_pipeline(&pending_pipeline) else {
                    gst::error!(
                        CAT,
                        "Failed to analyze pipeline '{}'",
                        pending_pipeline.name()
                    );
                    return glib::ControlFlow::Break;
                };

                for (bin_name, bin_graph) in &tree.bins {
                    // Build path from root to this bin
                    let mut bin_path = Vec::new();
                    let mut current_bin = Some(bin_name.as_str());

                    while let Some(bin) = current_bin {
                        bin_path.push(bin);
                        current_bin = tree.get_parent_bin(bin);
                    }

                    bin_path.reverse();
                    bin_path.insert(0, entity_path.as_str());

                    RerunTracer::log_graph(bin_name, bin_graph, &rec, &bin_path.join("/"));
                }

                glib::ControlFlow::Break
            });

            self.timeout_source_id = Some(source_id);
        }

        fn should_log_immediately(&mut self, pipeline: &gst::Pipeline) -> bool {
            let now = Instant::now();
            let is_final_state = matches!(
                pipeline.current_state(),
                gst::State::Playing | gst::State::Paused
            );

            if is_final_state {
                self.pending_logs.remove(pipeline);
                if let Some(source_id) = self.timeout_source_id.take() {
                    source_id.remove();
                }
                self.last_logged.insert(pipeline.clone(), now);
                return true;
            }

            if let Some(last_time) = self.last_logged.get(pipeline) {
                let rate_limit = Duration::from_millis(self.debounce_ms.saturating_mul(2));
                if now.duration_since(*last_time) < rate_limit {
                    return false;
                }
            }

            self.last_logged.insert(pipeline.clone(), now);
            true
        }
    }

    pub struct RerunTracer {
        pub state: RwLock<Option<TracingState>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RerunTracer {
        const NAME: &'static str = "RerunTracer";
        type Type = super::RerunTracer;
        type ParentType = gst::Tracer;

        fn new() -> Self {
            Self {
                state: RwLock::new(None),
            }
        }
    }

    impl ObjectImpl for RerunTracer {
        fn constructed(&self) {
            self.parent_constructed();

            let mut debounce_ms = 300u64;
            let mut spawn_viewer = true;
            let mut app_id: Option<String> = None;
            let mut entity_path = "pipelines".to_string();

            if let Some(params) = self.obj().property::<Option<String>>("params") {
                let structure = gst::Structure::from_str(&format!("params,{}", params))
                    .unwrap_or_else(|_| gst::Structure::new_empty("params"));

                if let Ok(d) = structure.get::<i32>("debounce") {
                    if d > 0 {
                        debounce_ms = d as u64;
                    }
                }

                if let Ok(sv) = structure.get::<bool>("spawn-viewer") {
                    spawn_viewer = sv;
                }

                if let Ok(id) = structure.get::<String>("app-id") {
                    app_id = Some(id);
                }

                if let Ok(ep) = structure.get::<String>("entity-path") {
                    entity_path = ep;
                }
            }

            let rec = if spawn_viewer {
                match app_id {
                    Some(id) => rerun::RecordingStreamBuilder::new(id.as_str())
                        .spawn()
                        .map_err(|e| gst::error!(CAT, imp = self, "Failed to spawn viewer: {}", e))
                        .ok(),
                    None => {
                        gst::error!(CAT, imp = self, "spawn-viewer requires app-id");
                        None
                    }
                }
            } else {
                None
            };

            *self.state.write().unwrap() = Some(TracingState::new(debounce_ms, rec, entity_path));

            self.register_hook(TracerHook::BinAddPost);
            self.register_hook(TracerHook::BinRemovePost);
            self.register_hook(TracerHook::ElementNew);
            self.register_hook(TracerHook::ElementAddPad);
            self.register_hook(TracerHook::ElementRemovePad);
            self.register_hook(TracerHook::ElementChangeStatePost);
            self.register_hook(TracerHook::PadLinkPost);
            self.register_hook(TracerHook::PadUnlinkPost);

            gst::info!(CAT, imp = self, "RerunTracer ready");
        }
    }

    impl GstObjectImpl for RerunTracer {}

    impl TracerImpl for RerunTracer {
        fn element_change_state_post(
            &self,
            ts: u64,
            element: &gst::Element,
            change: gst::StateChange,
            result: Result<gst::StateChangeSuccess, gst::StateChangeError>,
        ) {
            if result.is_ok() {
                gst::debug!(
                    CAT,
                    imp = self,
                    "Element {:?} changed state to {:?} at ts {}",
                    element.name(),
                    change,
                    ts
                );
                self.analyze_and_log_pipeline(element);
            }
        }

        fn element_new(&self, ts: u64, element: &gst::Element) {
            gst::debug!(
                CAT,
                imp = self,
                "New element {:?} at ts {}",
                element.name(),
                ts
            );
            self.analyze_and_log_pipeline(element);
        }

        fn bin_add_post(&self, ts: u64, bin: &gst::Bin, element: &gst::Element, success: bool) {
            if success {
                gst::debug!(
                    CAT,
                    imp = self,
                    "Added {:?} to bin {:?} at ts {}",
                    element.name(),
                    bin.name(),
                    ts
                );
                self.analyze_and_log_pipeline(bin.as_ref());
            }
        }

        fn pad_link_post(
            &self,
            ts: u64,
            src: &gst::Pad,
            sink: &gst::Pad,
            result: Result<gst::PadLinkSuccess, gst::PadLinkError>,
        ) {
            if result.is_ok() {
                gst::debug!(
                    CAT,
                    imp = self,
                    "Linked {:?} -> {:?} at ts {}",
                    src.name(),
                    sink.name(),
                    ts
                );
                if let Some(src_element) = src.parent_element() {
                    self.analyze_and_log_pipeline(&src_element);
                }
            }
        }

        fn bin_remove_post(&self, ts: u64, bin: &gst::Bin, success: bool) {
            if success {
                gst::debug!(
                    CAT,
                    imp = self,
                    "Removed element from bin {:?} at ts {}",
                    bin.name(),
                    ts
                );
                self.analyze_and_log_pipeline(bin.as_ref());
            }
        }

        fn element_add_pad(&self, ts: u64, element: &gst::Element, pad: &gst::Pad) {
            gst::debug!(
                CAT,
                imp = self,
                "Added pad {:?} to {:?} at ts {}",
                pad.name(),
                element.name(),
                ts
            );
            self.analyze_and_log_pipeline(element);
        }

        fn element_remove_pad(&self, ts: u64, element: &gst::Element, pad: &gst::Pad) {
            gst::debug!(
                CAT,
                imp = self,
                "Removed pad {:?} from {:?} at ts {}",
                pad.name(),
                element.name(),
                ts
            );
            self.analyze_and_log_pipeline(element);
        }

        fn pad_unlink_post(&self, ts: u64, src: &gst::Pad, sink: &gst::Pad, success: bool) {
            if success {
                gst::debug!(
                    CAT,
                    imp = self,
                    "Unlinked {:?} from {:?} at ts {}",
                    src.name(),
                    sink.name(),
                    ts
                );
                if let Some(src_element) = src.parent_element() {
                    self.analyze_and_log_pipeline(&src_element);
                }
            }
        }
    }

    impl RerunTracer {
        fn analyze_and_log_pipeline(&self, element: &gst::Element) {
            let mut current = element.clone();
            loop {
                // Try to downcast to Pipeline
                if let Ok(pipeline) = current.clone().downcast::<gst::Pipeline>() {
                    self.log_pipeline_graph(&pipeline);
                    return;
                }

                // Move up to parent
                current = match current
                    .parent()
                    .and_then(|p| p.downcast::<gst::Element>().ok())
                {
                    Some(parent) => parent,
                    None => return,
                };
            }
        }

        fn log_pipeline_graph(&self, pipeline: &gst::Pipeline) {
            let mut state_guard = self.state.write().unwrap();
            let Some(state) = state_guard.as_mut() else {
                return;
            };

            let Some(rec) = state.rec.clone() else {
                return;
            };

            let entity_path_prefix = state.entity_path.clone();
            let should_log_now = state.should_log_immediately(pipeline);

            if !should_log_now {
                state.schedule_deferred_log(pipeline, rec, entity_path_prefix);
                return;
            }

            drop(state_guard);

            // Analyze the pipeline and log each bin
            let Ok(tree) = analyze_pipeline(pipeline) else {
                gst::error!(CAT, "Failed to analyze pipeline '{}'", pipeline.name());
                return;
            };

            for (bin_name, bin_graph) in &tree.bins {
                // Build path from root to this bin
                let mut bin_path = Vec::new();
                let mut current_bin = Some(bin_name.as_str());

                while let Some(bin) = current_bin {
                    bin_path.push(bin);
                    current_bin = tree.get_parent_bin(bin);
                }

                bin_path.reverse();
                bin_path.insert(0, entity_path_prefix.as_str());

                Self::log_graph(bin_name, bin_graph, &rec, &bin_path.join("/"));
            }
        }

        fn compute_layout(
            graph: &petgraph::graph::DiGraph<crate::pipeline_graph::ElementInfo, ()>,
        ) -> Option<Vec<rerun::Position2D>> {
            use petgraph_layout::{LayeredLayout, LayoutEngine, Vec2};

            if graph.node_count() == 0 {
                return Some(Vec::new());
            }

            let engine = LayeredLayout::new(Vec2::new(100.0, 50.0));
            let sizes = |_node: petgraph::graph::NodeIndex| Vec2::new(20.0, 10.0);
            let positions = match engine.layout(graph, &sizes) {
                Ok(pos) => pos,
                Err(e) => {
                    gst::error!(CAT, "Failed to compute layout: {}", e);
                    return None;
                }
            };

            let rerun_positions: Vec<rerun::Position2D> = graph
                .node_indices()
                .filter_map(|node_idx| {
                    positions
                        .get(&node_idx)
                        .map(|pos| rerun::Position2D::new(pos.x, pos.y))
                })
                .collect();

            if rerun_positions.len() == graph.node_count() {
                Some(rerun_positions)
            } else {
                gst::error!(
                    CAT,
                    "Layout computation incomplete: got {} positions for {} nodes",
                    rerun_positions.len(),
                    graph.node_count()
                );
                None
            }
        }

        fn log_graph(
            bin_name: &str,
            bin_graph: &crate::pipeline_graph::BinGraph,
            rec: &rerun::RecordingStream,
            entity_path: &str,
        ) {
            if bin_graph.graph.node_count() == 0 {
                return;
            }

            // Build node_names and labels by iterating nodes in graph order
            let mut node_names = Vec::new();
            let mut labels = Vec::new();

            for node_idx in bin_graph.graph.node_indices() {
                let elem = &bin_graph.graph[node_idx];
                node_names.push(elem.name.clone());

                let label = if let Some(factory_name) = &elem.factory_name {
                    format!("{}\n({})", elem.name, factory_name)
                } else {
                    elem.name.clone()
                };
                labels.push(label);
            }

            // Compute layout positions using the existing graph
            let positions = Self::compute_layout(&bin_graph.graph);

            let mut nodes = rerun::GraphNodes::new(node_names).with_labels(labels);
            if let Some(positions) = positions {
                nodes = nodes.with_positions(positions)
            }

            if let Err(e) = rec.log(
                entity_path,
                &[
                    &nodes as &dyn rerun::AsComponents,
                    &rerun::GraphEdges::new(bin_graph.get_edges()).with_directed_edges(),
                ],
            ) {
                gst::error!(CAT, "Failed to log graph for bin '{}': {}", bin_name, e);
            }
        }
    }
}
