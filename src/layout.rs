//! The tiling tree that arranges panes. Generic over the leaf type so the
//! split/remove/resize rules can be tested without any UI.

use gpui::Axis;

/// Smallest share of a split one child may be dragged down to.
const MIN_FRACTION: f32 = 0.08;

/// The share of `extent` the first of two stacked sections gets when the
/// boundary between them is dragged to `offset`. Neither section goes below
/// `min`; `None` when there is not room for both at that size.
pub fn boundary_share(offset: f32, extent: f32, min: f32) -> Option<f32> {
    if !(min > 0.) || !(extent >= min * 2.) || !offset.is_finite() {
        return None;
    }
    Some(offset.clamp(min, extent - min) / extent)
}

/// Where a new pane goes relative to the one being split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Right,
    Down,
    Left,
}

impl Direction {
    pub fn axis(self) -> Axis {
        match self {
            Direction::Left | Direction::Right => Axis::Horizontal,
            Direction::Up | Direction::Down => Axis::Vertical,
        }
    }

    /// Whether the new pane comes before the existing one in layout order.
    fn before(self) -> bool {
        matches!(self, Direction::Up | Direction::Left)
    }
}

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

    /// Places `new` beside `target` on the given side, halving the target's
    /// space.
    pub fn split(&mut self, target: &T, new: T, direction: Direction, next_id: &mut usize) -> bool {
        let axis = direction.axis();
        match self {
            Node::Leaf(leaf) => {
                if leaf != target {
                    return false;
                }
                let old = Node::Leaf(leaf.clone());
                let new = Node::Leaf(new);
                *next_id += 1;
                *self = Node::Split {
                    id: *next_id,
                    axis,
                    children: if direction.before() {
                        vec![new, old]
                    } else {
                        vec![old, new]
                    },
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
                        let at = if direction.before() { index } else { index + 1 };
                        children.insert(at, Node::Leaf(new));
                        return true;
                    }
                }
                for child in children.iter_mut() {
                    if child.contains(target) {
                        return child.split(target, new, direction, next_id);
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
        assert!(tree.split(&1, 2, Direction::Right, &mut next_id));
        assert!(tree.split(&1, 3, Direction::Right, &mut next_id));
        assert_eq!(tree.leaves(), vec![1, 3, 2]);
        assert_eq!(sizes(&tree), vec![0.25, 0.25, 0.5]);
    }

    #[test]
    fn splitting_cross_axis_nests() {
        let mut next_id = 0;
        let mut tree = Node::Leaf(1);
        tree.split(&1, 2, Direction::Right, &mut next_id);
        assert!(tree.split(&2, 3, Direction::Down, &mut next_id));
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
        assert!(!tree.split(&9, 4, Direction::Down, &mut next_id));
    }

    #[test]
    fn splitting_left_or_up_puts_the_new_pane_first() {
        let mut next_id = 0;
        let mut tree = Node::Leaf(1);
        assert!(tree.split(&1, 2, Direction::Left, &mut next_id));
        assert_eq!(tree.leaves(), vec![2, 1]);
        // Same axis: the new pane lands just before its target.
        assert!(tree.split(&1, 3, Direction::Left, &mut next_id));
        assert_eq!(tree.leaves(), vec![2, 3, 1]);
        assert_eq!(sizes(&tree), vec![0.5, 0.25, 0.25]);
        // Cross axis: nests, with the new pane on top.
        assert!(tree.split(&3, 4, Direction::Up, &mut next_id));
        assert_eq!(tree.leaves(), vec![2, 4, 3, 1]);
        assert_eq!(Direction::Up.axis(), Axis::Vertical);
        assert_eq!(Direction::Right.axis(), Axis::Horizontal);
    }

    #[test]
    fn removing_collapses_and_redistributes() {
        let mut next_id = 0;
        let mut tree = Node::Leaf(1);
        tree.split(&1, 2, Direction::Right, &mut next_id);
        tree.split(&2, 3, Direction::Down, &mut next_id);
        assert!(tree.remove(&3));
        assert_eq!(tree.leaves(), vec![1, 2]);
        assert!(tree.remove(&1));
        assert_eq!(tree, Node::Leaf(2));
        assert!(!tree.remove(&2), "the last pane stays");

        let mut tree = Node::Leaf(1);
        tree.split(&1, 2, Direction::Right, &mut next_id);
        tree.split(&1, 3, Direction::Right, &mut next_id);
        tree.remove(&2);
        assert_eq!(sizes(&tree), vec![0.5, 0.5]);
    }

    #[test]
    fn resizing_moves_one_boundary_within_limits() {
        let mut next_id = 0;
        let mut tree = Node::Leaf(1);
        tree.split(&1, 2, Direction::Right, &mut next_id);
        tree.split(&2, 3, Direction::Right, &mut next_id);
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

    #[test]
    fn a_dragged_boundary_keeps_both_sections_usable() {
        assert_eq!(boundary_share(150., 600., 60.), Some(0.25));
        // Dragging past either end stops at the minimum height.
        assert_eq!(boundary_share(-40., 600., 60.), Some(0.1));
        assert_eq!(boundary_share(900., 600., 60.), Some(0.9));
        assert_eq!(boundary_share(60., 120., 60.), Some(0.5));
        // Too short to fit both: leave the split as it was.
        assert_eq!(boundary_share(50., 100., 60.), None);
        assert_eq!(boundary_share(f32::NAN, 600., 60.), None);
    }
}
