//! The tiling tree that arranges panes. Generic over the leaf type so the
//! split/remove/resize rules can be tested without any UI.

use gpui::Axis;

/// Smallest share of a split one child may be dragged down to.
const MIN_FRACTION: f32 = 0.08;

#[derive(Clone, Debug, PartialEq)]
pub enum Node<T> {
    Leaf(T),
    Split {
        id: usize,
        /// `Horizontal` lays children out left to right.
        axis: Axis,
        children: Vec<Node<T>>,
        /// Each child's share of the split; sums to 1.
        sizes: Vec<f32>,
    },
}

impl<T: Clone + PartialEq> Node<T> {
    /// Leaves in visual order (left to right, top to bottom).
    pub fn leaves(&self) -> Vec<T> {
        fn visit<T: Clone>(node: &Node<T>, out: &mut Vec<T>) {
            match node {
                Node::Leaf(leaf) => out.push(leaf.clone()),
                Node::Split { children, .. } => children.iter().for_each(|child| visit(child, out)),
            }
        }
        let mut out = Vec::new();
        visit(self, &mut out);
        out
    }

    pub fn contains(&self, target: &T) -> bool {
        match self {
            Node::Leaf(leaf) => leaf == target,
            Node::Split { children, .. } => children.iter().any(|child| child.contains(target)),
        }
    }

    fn is_leaf(&self, target: &T) -> bool {
        matches!(self, Node::Leaf(leaf) if leaf == target)
    }

    /// Places `new` next to `target` along `axis`, halving the target's space.
    pub fn split(&mut self, target: &T, new: T, axis: Axis, next_id: &mut usize) -> bool {
        match self {
            Node::Leaf(leaf) => {
                if leaf != target {
                    return false;
                }
                let old = Node::Leaf(leaf.clone());
                *next_id += 1;
                *self = Node::Split {
                    id: *next_id,
                    axis,
                    children: vec![old, Node::Leaf(new)],
                    sizes: vec![0.5, 0.5],
                };
                true
            }
            Node::Split {
                axis: split_axis,
                children,
                sizes,
                ..
            } => {
                if *split_axis == axis {
                    if let Some(index) = children.iter().position(|child| child.is_leaf(target)) {
                        let half = sizes[index] / 2.;
                        sizes[index] = half;
                        sizes.insert(index + 1, half);
                        children.insert(index + 1, Node::Leaf(new));
                        return true;
                    }
                }
                for child in children.iter_mut() {
                    if child.contains(target) {
                        return child.split(target, new, axis, next_id);
                    }
                }
                false
            }
        }
    }

    /// Removes a leaf, giving its space to its siblings. The root leaf cannot
    /// be removed.
    pub fn remove(&mut self, target: &T) -> bool {
        let Node::Split {
            children, sizes, ..
        } = self
        else {
            return false;
        };
        if let Some(index) = children.iter().position(|child| child.is_leaf(target)) {
            children.remove(index);
            let freed = sizes.remove(index);
            let remaining: f32 = sizes.iter().sum();
            if remaining > 0. {
                for size in sizes.iter_mut() {
                    *size += freed * (*size / remaining);
                }
            }
            if children.len() == 1 {
                let only = children.pop().expect("one child");
                *self = only;
            }
            return true;
        }
        children.iter_mut().any(|child| child.remove(target))
    }

    /// Moves the boundary after child `index` of split `split_id` to
    /// `fraction` of the split's extent.
    pub fn resize(&mut self, split_id: usize, index: usize, fraction: f32) -> bool {
        let Node::Split {
            id,
            children,
            sizes,
            ..
        } = self
        else {
            return false;
        };
        if *id != split_id {
            return children
                .iter_mut()
                .any(|child| child.resize(split_id, index, fraction));
        }
        if index + 1 >= sizes.len() {
            return false;
        }
        let before: f32 = sizes[..index].iter().sum();
        let pair = sizes[index] + sizes[index + 1];
        let min = MIN_FRACTION.min(pair / 2.);
        let first = (fraction - before).clamp(min, pair - min);
        if (first - sizes[index]).abs() < 0.0005 {
            return false;
        }
        sizes[index] = first;
        sizes[index + 1] = pair - first;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sizes(node: &Node<u32>) -> Vec<f32> {
        match node {
            Node::Split { sizes, .. } => sizes.clone(),
            Node::Leaf(_) => vec![],
        }
    }

    #[test]
    fn splitting_same_axis_adds_a_sibling() {
        let mut next_id = 0;
        let mut tree = Node::Leaf(1);
        assert!(tree.split(&1, 2, Axis::Horizontal, &mut next_id));
        assert!(tree.split(&1, 3, Axis::Horizontal, &mut next_id));
        assert_eq!(tree.leaves(), vec![1, 3, 2]);
        assert_eq!(sizes(&tree), vec![0.25, 0.25, 0.5]);
    }

    #[test]
    fn splitting_cross_axis_nests() {
        let mut next_id = 0;
        let mut tree = Node::Leaf(1);
        tree.split(&1, 2, Axis::Horizontal, &mut next_id);
        assert!(tree.split(&2, 3, Axis::Vertical, &mut next_id));
        assert_eq!(tree.leaves(), vec![1, 2, 3]);
        let Node::Split { children, .. } = &tree else {
            panic!()
        };
        assert!(matches!(
            &children[1],
            Node::Split {
                axis: Axis::Vertical,
                ..
            }
        ));
        assert!(!tree.split(&9, 4, Axis::Vertical, &mut next_id));
    }

    #[test]
    fn removing_collapses_and_redistributes() {
        let mut next_id = 0;
        let mut tree = Node::Leaf(1);
        tree.split(&1, 2, Axis::Horizontal, &mut next_id);
        tree.split(&2, 3, Axis::Vertical, &mut next_id);
        assert!(tree.remove(&3));
        assert_eq!(tree.leaves(), vec![1, 2]);
        assert!(tree.remove(&1));
        assert_eq!(tree, Node::Leaf(2));
        assert!(!tree.remove(&2), "the last pane stays");

        let mut tree = Node::Leaf(1);
        tree.split(&1, 2, Axis::Horizontal, &mut next_id);
        tree.split(&1, 3, Axis::Horizontal, &mut next_id);
        tree.remove(&2);
        assert_eq!(sizes(&tree), vec![0.5, 0.5]);
    }

    #[test]
    fn resizing_moves_one_boundary_within_limits() {
        let mut next_id = 0;
        let mut tree = Node::Leaf(1);
        tree.split(&1, 2, Axis::Horizontal, &mut next_id);
        tree.split(&2, 3, Axis::Horizontal, &mut next_id);
        // sizes: [0.5, 0.25, 0.25]
        assert!(tree.resize(1, 1, 0.6));
        let after = sizes(&tree);
        assert!((after[0] - 0.5).abs() < 1e-6);
        assert!((after[1] - 0.1).abs() < 1e-6);
        assert!((after[2] - 0.4).abs() < 1e-6);
        // Dragging past the neighbour clamps to the minimum size.
        tree.resize(1, 0, 0.0);
        assert!((sizes(&tree)[0] - MIN_FRACTION).abs() < 1e-6);
        assert!(!tree.resize(99, 0, 0.5));
    }
}
