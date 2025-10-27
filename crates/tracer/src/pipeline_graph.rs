//! Pipeline Graph Traversal Module
//!
//! This module provides functionality to traverse GStreamer pipeline graphs,
//! extracting element hierarchy and link relationships for visualization.
//! Each Bin gets its own petgraph, organized in a tree structure.

use gst::prelude::*;
use gst::{Bin, IteratorError};
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::HashMap;

/// Element information stored in the graph
#[derive(Debug, Clone)]
pub struct ElementInfo {
    /// Element name
    pub name: String,
    /// Element factory name (e.g., "fakesrc"), if available
    pub factory_name: Option<String>,
}

/// A single bin's graph representation
#[derive(Debug, Default)]
pub struct BinGraph {
    /// The bin's name
    pub bin_name: String,
    /// Graph of direct child elements within this bin
    pub graph: DiGraph<ElementInfo, ()>,
    /// Map from element name to node index for quick lookup
    pub node_indices: HashMap<String, NodeIndex>,
}

/// Tree structure holding all bins and their relationships
#[derive(Debug, Default)]
pub struct PipelineTree {
    /// All bin graphs, keyed by bin name
    pub bins: HashMap<String, BinGraph>,
    /// Parent-child relationships: child_bin_name -> parent_bin_name
    pub hierarchy: HashMap<String, String>,
    /// Root bin name (the pipeline itself)
    pub root: String,
}

/// Graph traverser that builds one graph per Bin
pub struct PipelineTreeBuilder {
    tree: PipelineTree,
}

impl PipelineTreeBuilder {
    pub fn new() -> Self {
        Self {
            tree: PipelineTree::default(),
        }
    }

    /// Traverse the pipeline and build a tree of bin graphs
    pub fn build_tree(
        &mut self,
        pipeline: &impl IsA<Bin>,
    ) -> Result<PipelineTree, Box<dyn std::error::Error>> {
        let pipeline_name = pipeline.as_ref().name().to_string();
        self.tree.root = pipeline_name.clone();

        // Process the root pipeline/bin
        self.process_bin(pipeline, None)?;

        Ok(std::mem::take(&mut self.tree))
    }

    /// Process a single bin: create its graph and recursively process child bins
    fn process_bin(
        &mut self,
        bin: &impl IsA<Bin>,
        parent_bin_name: Option<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let bin_name = bin.as_ref().name().to_string();

        // Create a new graph for this bin
        let mut bin_graph = BinGraph::default();
        bin_graph.bin_name = bin_name.clone();

        // Record hierarchy
        if let Some(parent) = parent_bin_name {
            self.tree.hierarchy.insert(bin_name.clone(), parent);
        }

        // Iterate over direct children of this bin
        let mut element_iter = bin.iterate_elements();
        loop {
            match element_iter.next() {
                Ok(Some(element)) => {
                    let element_name = element.name().to_string();
                    let factory_name = element.factory().map(|f| f.name().to_string());

                    // Add this element as a node in the current bin's graph
                    let element_info = ElementInfo {
                        name: element_name.clone(),
                        factory_name,
                    };
                    let node_idx = bin_graph.graph.add_node(element_info);
                    bin_graph.node_indices.insert(element_name.clone(), node_idx);

                    // If this element is also a bin, recursively process it
                    if element.is::<Bin>() {
                        let child_bin = element.clone().downcast::<Bin>().unwrap();
                        self.process_bin(&child_bin, Some(bin_name.clone()))?;
                    }
                }
                Ok(None) => break,
                Err(IteratorError::Resync) => {
                    element_iter.resync();
                    continue;
                }
                Err(IteratorError::Error) => {
                    return Err("Element iterator error".into());
                }
            }
        }

        // Now discover links between elements in this bin
        self.discover_links_in_bin(bin, &mut bin_graph)?;

        // Store the completed bin graph
        self.tree.bins.insert(bin_name, bin_graph);

        Ok(())
    }

    /// Discover links between elements within a single bin
    fn discover_links_in_bin(
        &self,
        bin: &impl IsA<Bin>,
        bin_graph: &mut BinGraph,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut element_iter = bin.iterate_elements();

        loop {
            match element_iter.next() {
                Ok(Some(element)) => {
                    let element_name = element.name().to_string();
                    let mut pad_iter = element.iterate_pads();

                    loop {
                        match pad_iter.next() {
                            Ok(Some(pad)) => {
                                // Only process source pads to avoid duplicate links
                                if pad.is_linked() && pad.direction() == gst::PadDirection::Src {
                                    if let Some(peer_pad) = pad.peer() {
                                        if let Some(peer_element) = peer_pad.parent_element() {
                                            let peer_name = peer_element.name().to_string();

                                            // Only add edge if both elements are in this bin
                                            if let (Some(&src_idx), Some(&sink_idx)) = (
                                                bin_graph.node_indices.get(&element_name),
                                                bin_graph.node_indices.get(&peer_name),
                                            ) {
                                                bin_graph.graph.add_edge(src_idx, sink_idx, ());
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(None) => break,
                            Err(IteratorError::Resync) => {
                                pad_iter.resync();
                                continue;
                            }
                            Err(IteratorError::Error) => {
                                return Err("Pad iterator error".into());
                            }
                        }
                    }
                }
                Ok(None) => break,
                Err(IteratorError::Resync) => {
                    element_iter.resync();
                    continue;
                }
                Err(IteratorError::Error) => {
                    return Err("Element iterator error".into());
                }
            }
        }

        Ok(())
    }
}

impl Default for PipelineTreeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl PipelineTree {
    /// Get a bin graph by name
    pub fn get_bin(&self, bin_name: &str) -> Option<&BinGraph> {
        self.bins.get(bin_name)
    }

    /// Get the parent bin of a given bin
    pub fn get_parent_bin(&self, bin_name: &str) -> Option<&str> {
        self.hierarchy.get(bin_name).map(|s| s.as_str())
    }
}

impl BinGraph {
    /// Get edges as (source_name, target_name) tuples
    pub fn get_edges(&self) -> Vec<(String, String)> {
        use petgraph::visit::EdgeRef;

        self.graph
            .edge_references()
            .map(|edge| {
                let src = &self.graph[edge.source()];
                let sink = &self.graph[edge.target()];
                (src.name.clone(), sink.name.clone())
            })
            .collect()
    }
}

/// Analyze a pipeline and return its tree structure
pub fn analyze_pipeline(
    pipeline: &impl IsA<Bin>,
) -> Result<PipelineTree, Box<dyn std::error::Error>> {
    let mut builder = PipelineTreeBuilder::new();
    builder.build_tree(pipeline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_pipeline() {
        crate::tests::init();

        let pipeline_str = "fakesrc ! fakesink";
        let pipeline = gst::parse::launch(pipeline_str)
            .expect("Failed to parse pipeline")
            .downcast::<gst::Pipeline>()
            .expect("Expected a pipeline");

        let tree = analyze_pipeline(&pipeline).expect("Failed to analyze pipeline");

        // Should have one bin (the pipeline itself)
        assert_eq!(tree.bins.len(), 1);

        // Get the root bin
        let root_bin = tree.get_bin(&tree.root).expect("Should have root bin");

        // Should have 2 elements
        assert_eq!(root_bin.graph.node_count(), 2);
        // Should have 1 edge
        assert_eq!(root_bin.graph.edge_count(), 1);

        // Check that we have a fakesrc and fakesink
        let factories: Vec<_> = root_bin
            .graph
            .node_weights()
            .filter_map(|data| data.factory_name.as_ref())
            .collect();
        assert!(factories.contains(&&"fakesrc".to_string()));
        assert!(factories.contains(&&"fakesink".to_string()));
    }

    #[test]
    fn test_pipeline_with_bins() {
        crate::tests::init();

        let pipeline_str = "fakesrc ! bin.( queue ! fakesink )";

        let pipeline = gst::parse::launch(pipeline_str)
            .expect("Failed to parse pipeline")
            .downcast::<gst::Pipeline>()
            .expect("Expected a pipeline");

        let tree = analyze_pipeline(&pipeline).expect("Failed to analyze pipeline");

        // Should have 2 bins: the pipeline and the nested bin
        assert_eq!(tree.bins.len(), 2);

        // Get the root bin
        let root_bin = tree.get_bin(&tree.root).expect("Should have root bin");

        // Root should have 2 elements: fakesrc and bin
        assert_eq!(root_bin.graph.node_count(), 2);

        // Find the nested bin name
        let nested_bin_name = root_bin
            .graph
            .node_weights()
            .find(|elem| elem.name.starts_with("bin"))
            .map(|elem| elem.name.as_str())
            .expect("Should have a nested bin");

        // Get the nested bin graph
        let nested_bin = tree.get_bin(nested_bin_name).expect("Should have nested bin graph");

        // Nested bin should have 2 elements: queue and fakesink
        assert_eq!(nested_bin.graph.node_count(), 2);
        assert_eq!(nested_bin.graph.edge_count(), 1);

        // Verify hierarchy
        assert_eq!(tree.get_parent_bin(nested_bin_name), Some(tree.root.as_str()));
    }

    #[test]
    fn test_unlinked_elements() {
        crate::tests::init();

        let pipeline = gst::Pipeline::new();
        let src = gst::ElementFactory::make("fakesrc").build().unwrap();
        let sink = gst::ElementFactory::make("fakesink").build().unwrap();

        pipeline.add(&src).unwrap();
        pipeline.add(&sink).unwrap();
        // Intentionally not linking them

        let tree = analyze_pipeline(&pipeline).expect("Failed to analyze pipeline");

        // Should have one bin (the pipeline)
        assert_eq!(tree.bins.len(), 1);

        let root_bin = tree.get_bin(&tree.root).expect("Should have root bin");

        assert_eq!(root_bin.graph.node_count(), 2);
        assert_eq!(root_bin.graph.edge_count(), 0); // No links because elements aren't connected
    }
}
