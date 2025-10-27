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
            rec: Option<rerun::RecordingStream>,
            entity_path: String,
        ) {
            let Some(rec) = rec else {
                gst::warning!(CAT, "Cannot schedule deferred log without a recorder");
                return;
            };

            self.pending_logs.insert(pipeline.clone());

            if let Some(source_id) = self.timeout_source_id.take() {
                source_id.remove();
            }

            let pending_pipeline = pipeline.clone();
            let debounce_duration = Duration::from_millis(self.debounce_ms);
            let source_id = glib::timeout_add(debounce_duration, move || {
                RerunTracer::log_pipeline(&pending_pipeline, &rec, &entity_path);
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
                if current.type_().name() == "GstPipeline" {
                    if let Ok(pipeline) = current.clone().downcast::<gst::Pipeline>() {
                        self.log_pipeline_graph(&pipeline);
                        return;
                    }
                }

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

            let entity_path = state.entity_path.clone();
            let should_log_now = state.should_log_immediately(pipeline);

            if !should_log_now {
                state.schedule_deferred_log(pipeline, Some(rec), entity_path);
                return;
            }

            drop(state_guard);
            Self::log_pipeline(pipeline, &rec, &entity_path);
        }

        fn log_pipeline(pipeline: &gst::Pipeline, rec: &rerun::RecordingStream, entity_path: &str) {
            let Ok(graph) = analyze_pipeline(pipeline) else {
                gst::error!(CAT, "Failed to analyze pipeline '{}'", pipeline.name());
                return;
            };

            Self::log_graph(pipeline.name().as_str(), None, &graph, rec, entity_path);
        }

        fn compute_layout(
            node_names: &[String],
            edges: &[(String, String)],
        ) -> Option<Vec<rerun::Position2D>> {
            use petgraph::graphmap::DiGraphMap;
            use petgraph_layout::{LayeredLayout, LayoutEngine, Vec2};

            // Build a petgraph from the node names and edges
            let mut pg = DiGraphMap::<&str, ()>::new();

            // Add all nodes
            for name in node_names {
                pg.add_node(name.as_str());
            }

            // Add all edges
            for (src, sink) in edges {
                pg.add_edge(src.as_str(), sink.as_str(), ());
            }

            // Create layout engine with spacing parameters
            let engine = LayeredLayout::new(Vec2::new(150.0, 100.0));

            // Define node sizes (uniform for now)
            let sizes = |_node: &str| Vec2::new(10.0, 5.0);

            // Compute layout
            let positions = match engine.layout(&pg, &sizes) {
                Ok(pos) => pos,
                Err(e) => {
                    gst::warning!(CAT, "Failed to compute layout: {}", e);
                    return None;
                }
            };

            // Convert positions to rerun format, maintaining node order
            let rerun_positions: Vec<rerun::Position2D> = node_names
                .iter()
                .filter_map(|name| {
                    positions
                        .get(&name.as_str())
                        .map(|pos| rerun::Position2D::new(pos.x, pos.y))
                })
                .collect();

            if rerun_positions.len() == node_names.len() {
                Some(rerun_positions)
            } else {
                gst::warning!(
                    CAT,
                    "Layout computation incomplete: got {} positions for {} nodes",
                    rerun_positions.len(),
                    node_names.len()
                );
                None
            }
        }

        fn log_graph(
            pipeline_name: &str,
            parent_name: Option<&str>,
            graph: &crate::pipeline_graph::PipelineGraph,
            rec: &rerun::RecordingStream,
            entity_path_prefix: &str,
        ) {
            let nodes = if let Some(parent) = parent_name {
                graph.get_children(parent)
            } else {
                graph.get_root_nodes()
            };

            if nodes.is_empty() {
                return;
            }

            let entity_path = if let Some(parent) = parent_name {
                format!("{}/{}/{}", entity_path_prefix, pipeline_name, parent)
            } else {
                format!("{}/{}", entity_path_prefix, pipeline_name)
            };

            let node_names: Vec<String> = nodes.iter().map(|n| n.name.clone()).collect();

            let labels: Vec<String> = nodes
                .iter()
                .map(|node| {
                    if let Some(factory_name) = &node.factory_name {
                        format!("{}\n({})", node.name, factory_name)
                    } else {
                        node.name.clone()
                    }
                })
                .collect();

            let edges: Vec<(String, String)> = graph
                .links
                .iter()
                .filter(|(src, sink)| node_names.contains(src) && node_names.contains(sink))
                .cloned()
                .collect();

            // Compute layout positions using petgraph-layout
            let positions = Self::compute_layout(&node_names, &edges);

            let graph_nodes = if let Some(positions) = positions {
                rerun::GraphNodes::new(node_names.clone())
                    .with_labels(labels)
                    .with_positions(positions)
            } else {
                rerun::GraphNodes::new(node_names.clone()).with_labels(labels)
            };

            if let Err(e) = rec.log(
                entity_path.as_str(),
                &[
                    &graph_nodes as &dyn rerun::AsComponents,
                    &rerun::GraphEdges::new(edges).with_directed_edges(),
                ],
            ) {
                gst::error!(CAT, "Failed to log graph: {}", e);
            }

            for node in nodes {
                if !graph.get_children(&node.name).is_empty() {
                    Self::log_graph(
                        pipeline_name,
                        Some(&node.name),
                        graph,
                        rec,
                        entity_path_prefix,
                    );
                }
            }
        }
    }
}
