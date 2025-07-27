//! Pipeline Graph Traversal Module
//!
//! This module provides functionality to traverse GStreamer pipeline graphs,
//! extracting element hierarchy and link relationships for visualization.
//! It implements the same traversal strategy as GStreamer's dot file generation.

use gst::prelude::*;
use gst::{Bin, Element, IteratorError};
use std::collections::HashMap;

/// Represents a single element in the pipeline graph
#[derive(Debug, Clone)]
pub struct GraphNode {
    /// Element name (unique within its parent bin)
    pub name: String,
    /// Element factory name (e.g., "fakesrc"), if available
    pub factory_name: Option<String>,
    /// Name of parent element (for nested bins)
    pub parent: Option<String>,
    /// Depth in the element hierarchy (0 = top level)
    pub depth: usize,
}

/// Complete graph representation of a GStreamer pipeline
#[derive(Debug, Default)]
pub struct PipelineGraph {
    /// All elements in the pipeline, indexed by name
    pub nodes: HashMap<String, GraphNode>,
    /// All links between elements (src_element, sink_element)
    pub links: Vec<(String, String)>,
}

/// Graph traverser that implements the GStreamer pipeline analysis algorithm
///
/// This struct performs the actual traversal work, following the same strategy
/// as GStreamer's dot file generation but collecting structured data instead.
pub struct PipelineGraphTraverser {
    graph: PipelineGraph,
}

impl PipelineGraphTraverser {
    pub fn new() -> Self {
        Self {
            graph: PipelineGraph::default(),
        }
    }

    /// Main traversal function - equivalent to debug_dump_element
    pub fn traverse_bin(
        &mut self,
        bin: &impl IsA<Bin>,
    ) -> Result<PipelineGraph, Box<dyn std::error::Error>> {
        self.traverse_bin_recursive(bin, None, 0)?;
        Ok(std::mem::take(&mut self.graph))
    }

    fn traverse_bin_recursive(
        &mut self,
        bin: &impl IsA<Bin>,
        parent_name: Option<String>,
        depth: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut element_iter = bin.iterate_elements();

        loop {
            match element_iter.next() {
                Ok(Some(element)) => {
                    self.process_element(&element, parent_name.clone(), depth)?;

                    // Recurse if element is also a bin
                    if element.is::<Bin>() {
                        let child_bin = element.clone().downcast::<Bin>().unwrap();
                        let element_name = element.name().to_string();
                        self.traverse_bin_recursive(&child_bin, Some(element_name), depth + 1)?;
                    }
                }
                Ok(None) => break, // Iterator finished
                Err(IteratorError::Resync) => {
                    // Pipeline changed during iteration, restart
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

    fn process_element(
        &mut self,
        element: &Element,
        parent_name: Option<String>,
        depth: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let element_name = element.name().to_string();
        let factory_name = element.factory().map(|f| f.name().to_string());

        // Store node info
        let node = GraphNode {
            name: element_name.clone(),
            factory_name,
            parent: parent_name,
            depth,
        };
        self.graph.nodes.insert(element_name, node);

        // Discover links by iterating pads
        self.discover_links(element)?;

        Ok(())
    }

    fn discover_links(&mut self, element: &Element) -> Result<(), Box<dyn std::error::Error>> {
        let mut pad_iter = element.iterate_pads();
        let element_name = element.name().to_string();

        loop {
            match pad_iter.next() {
                Ok(Some(pad)) => {
                    // Only process source pads to avoid duplicate links
                    if pad.is_linked() && pad.direction() == gst::PadDirection::Src {
                        if let Some(peer_pad) = pad.peer() {
                            if let Some(peer_element) = peer_pad.parent_element() {
                                let link = (element_name.clone(), peer_element.name().to_string());
                                self.graph.links.push(link);
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
                    return Err("Link discovery iterator error".into());
                }
            }
        }

        Ok(())
    }
}

impl Default for PipelineGraphTraverser {
    fn default() -> Self {
        Self::new()
    }
}

impl PipelineGraph {
    /// Get all elements that are children of the specified parent
    pub fn get_children(&self, parent_name: &str) -> Vec<&GraphNode> {
        self.nodes
            .values()
            .filter(|node| node.parent.as_deref() == Some(parent_name))
            .collect()
    }

    /// Get all root-level elements (no parent)
    pub fn get_root_nodes(&self) -> Vec<&GraphNode> {
        self.nodes
            .values()
            .filter(|node| node.parent.is_none())
            .collect()
    }
}

/// Analyze a pipeline and return its graph structure
pub fn analyze_pipeline(
    pipeline: &impl IsA<Bin>,
) -> Result<PipelineGraph, Box<dyn std::error::Error>> {
    let mut traverser = PipelineGraphTraverser::new();
    traverser.traverse_bin(pipeline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_pipeline() {
        crate::tests::init_test();

        let pipeline_str = "fakesrc ! fakesink";
        let pipeline = gst::parse::launch(pipeline_str)
            .expect("Failed to parse pipeline")
            .downcast::<gst::Pipeline>()
            .expect("Expected a pipeline");

        let graph = analyze_pipeline(&pipeline).expect("Failed to analyze pipeline");

        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.links.len(), 1);

        // Check that we have a fakesrc and fakesink
        let factories: Vec<_> = graph
            .nodes
            .values()
            .filter_map(|node| node.factory_name.as_ref())
            .collect();
        assert!(factories.contains(&&"fakesrc".to_string()));
        assert!(factories.contains(&&"fakesink".to_string()));
    }

    #[test]
    fn test_pipeline_with_bins() {
        crate::tests::init_test();

        let pipeline_str = "fakesrc ! bin.( queue ! fakesink )";

        let pipeline = gst::parse::launch(pipeline_str)
            .expect("Failed to parse pipeline")
            .downcast::<gst::Pipeline>()
            .expect("Expected a pipeline");

        let graph = analyze_pipeline(&pipeline).expect("Failed to analyze pipeline");

        // Should have: fakesrc, bin, queue, fakesink
        assert_eq!(graph.nodes.len(), 4);

        // Find the bin
        let bin_node = graph
            .nodes
            .values()
            .find(|node| node.name.starts_with("bin"))
            .expect("Should have a bin");

        assert_eq!(bin_node.depth, 0);

        // Verify it's actually a bin by checking it has children
        assert!(!graph.get_children(&bin_node.name).is_empty());

        // Find elements inside the bin
        let children = graph.get_children(&bin_node.name);
        assert_eq!(children.len(), 2); // queue and fakesink

        // Check depths
        for child in children {
            assert_eq!(child.depth, 1);
            assert_eq!(child.parent.as_ref().unwrap(), &bin_node.name);
        }
    }

    #[test]
    fn test_unlinked_elements() {
        crate::tests::init_test();

        let pipeline = gst::Pipeline::new();
        let src = gst::ElementFactory::make("fakesrc").build().unwrap();
        let sink = gst::ElementFactory::make("fakesink").build().unwrap();

        pipeline.add(&src).unwrap();
        pipeline.add(&sink).unwrap();
        // Intentionally not linking them

        let graph = analyze_pipeline(&pipeline).expect("Failed to analyze pipeline");

        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.links.len(), 0); // No links because elements aren't connected
    }
}
