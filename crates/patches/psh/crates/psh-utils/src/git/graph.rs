//! log --graph: the history graph drawn beside each commit, a port of git's graph.c
//! (without colors), and the topological order it draws commits in.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::repo::Commit;
use super::store::Oid;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Padding,
    Skip,
    PreCommit,
    Commit,
    PostMerge,
    Collapsing,
}

const MERGE_CHARS: &[u8; 3] = b"/|\\";

/// git's sort_in_topological_order in graph order: no parent before all its children,
/// each line of history kept together (the last parent's first), the tips in the
/// order the walk gave them.
pub fn topo_sort(list: Vec<Rc<Commit>>, first_parent: bool) -> Vec<Rc<Commit>> {
    let parents = |c: &Commit| -> Vec<Oid> {
        let n = if first_parent { 1 } else { c.parents.len() };
        c.parents.iter().take(n).copied().collect()
    };
    let mut indegree: HashMap<Oid, usize> = list.iter().map(|c| (c.id, 1)).collect();
    for c in &list {
        for p in parents(c) {
            if let Some(n) = indegree.get_mut(&p) {
                *n += 1;
            }
        }
    }
    let by_id: HashMap<Oid, Rc<Commit>> = list.iter().map(|c| (c.id, c.clone())).collect();
    // A stack, with the walk's first tip on top.
    let mut stack: Vec<Rc<Commit>> = list
        .iter()
        .filter(|c| indegree[&c.id] == 1)
        .cloned()
        .collect();
    stack.reverse();
    let mut out = Vec::with_capacity(list.len());
    while let Some(c) = stack.pop() {
        for p in parents(&c) {
            let Some(n) = indegree.get_mut(&p) else {
                continue;
            };
            if *n == 0 {
                continue;
            }
            *n -= 1;
            if *n == 1 {
                stack.push(by_id[&p].clone());
            }
        }
        indegree.insert(c.id, 0);
        out.push(c);
    }
    out
}

pub struct Graph {
    /// The commits log shows: only edges to them are drawn.
    interesting: HashSet<Oid>,
    first_parent: bool,
    commit: Option<Oid>,
    /// The current commit's interesting parents.
    parents: Vec<Oid>,
    width: usize,
    expansion_row: usize,
    state: State,
    prev_state: State,
    commit_index: usize,
    prev_commit_index: usize,
    merge_layout: i32,
    edges_added: i32,
    prev_edges_added: i32,
    columns: Vec<Oid>,
    new_columns: Vec<Oid>,
    mapping: Vec<i32>,
    old_mapping: Vec<i32>,
    mapping_size: usize,
}

impl Graph {
    pub fn new(interesting: HashSet<Oid>, first_parent: bool) -> Graph {
        Graph {
            interesting,
            first_parent,
            commit: None,
            parents: Vec::new(),
            width: 0,
            expansion_row: 0,
            state: State::Padding,
            prev_state: State::Padding,
            commit_index: 0,
            prev_commit_index: 0,
            merge_layout: 0,
            edges_added: 0,
            prev_edges_added: 0,
            columns: Vec::new(),
            new_columns: Vec::new(),
            mapping: Vec::new(),
            old_mapping: Vec::new(),
            mapping_size: 0,
        }
    }

    fn num_parents(&self) -> usize {
        self.parents.len()
    }

    fn is_commit(&self, oid: &Oid) -> bool {
        self.commit.as_ref() == Some(oid)
    }

    pub fn update(&mut self, commit: &Commit) {
        self.commit = Some(commit.id);
        self.parents.clear();
        for (i, p) in commit.parents.iter().enumerate() {
            if self.first_parent && i > 0 {
                break;
            }
            if self.interesting.contains(p) {
                self.parents.push(*p);
            }
        }
        self.prev_commit_index = self.commit_index;
        self.update_columns(commit.id);
        self.expansion_row = 0;
        self.state = if self.state != State::Padding {
            State::Skip
        } else if self.needs_pre_commit_line() {
            State::PreCommit
        } else {
            State::Commit
        };
    }

    fn update_state(&mut self, s: State) {
        self.prev_state = self.state;
        self.state = s;
    }

    fn insert_into_new_columns(&mut self, oid: Oid, idx: Option<usize>) {
        let i = match self.new_columns.iter().position(|c| *c == oid) {
            Some(i) => i,
            None => {
                self.new_columns.push(oid);
                self.new_columns.len() - 1
            }
        };
        let mapping_idx;
        if self.num_parents() > 1
            && let Some(idx) = idx
            && self.merge_layout == -1
        {
            let dist = idx as i32 - i as i32;
            let shift = if dist > 1 { 2 * dist - 3 } else { 1 };
            self.merge_layout = if dist > 0 { 0 } else { 1 };
            self.edges_added = self.num_parents() as i32 + self.merge_layout - 2;
            mapping_idx = (self.width as i32 + (self.merge_layout - 1) * shift) as usize;
            self.width += 2 * self.merge_layout as usize;
        } else if self.edges_added > 0
            && self.width >= 2
            && self.mapping[self.width - 2] == i as i32
        {
            mapping_idx = self.width - 2;
            self.edges_added = -1;
        } else {
            mapping_idx = self.width;
            self.width += 2;
        }
        self.mapping[mapping_idx] = i as i32;
    }

    fn update_columns(&mut self, commit: Oid) {
        std::mem::swap(&mut self.columns, &mut self.new_columns);
        self.new_columns.clear();
        let max_new_columns = self.columns.len() + self.num_parents();
        self.mapping_size = 2 * max_new_columns;
        let len = self.mapping_size.max(self.mapping.len());
        self.mapping.clear();
        self.mapping.resize(len, -1);
        self.old_mapping.resize(len, -1);
        self.width = 0;
        self.prev_edges_added = self.edges_added;
        self.edges_added = 0;
        let mut seen_this = false;
        let num_columns = self.columns.len();
        for i in 0..=num_columns {
            let col = if i == num_columns {
                if seen_this {
                    break;
                }
                commit
            } else {
                self.columns[i]
            };
            if col == commit {
                seen_this = true;
                self.commit_index = i;
                self.merge_layout = -1;
                for p in self.parents.clone() {
                    self.insert_into_new_columns(p, Some(i));
                }
                if self.num_parents() == 0 {
                    self.width += 2;
                }
            } else {
                self.insert_into_new_columns(col, None);
            }
        }
        while self.mapping_size > 1 && self.mapping[self.mapping_size - 1] < 0 {
            self.mapping_size -= 1;
        }
    }

    fn num_dashed_parents(&self) -> i32 {
        self.num_parents() as i32 + self.merge_layout - 3
    }

    fn needs_pre_commit_line(&self) -> bool {
        self.num_parents() >= 3
            && self.commit_index + 1 < self.columns.len()
            && (self.expansion_row as i32) < self.num_dashed_parents() * 2
    }

    fn mapping_correct(&self) -> bool {
        (0..self.mapping_size).all(|i| {
            let t = self.mapping[i];
            t < 0 || t as usize == i / 2
        })
    }

    fn finished(&self) -> bool {
        self.state == State::Padding
    }

    /// The next line of the graph, padded to its width, and whether it was the
    /// commit's own line.
    fn next_line(&mut self) -> (Vec<u8>, bool) {
        let mut line = Vec::new();
        let mut commit = false;
        match self.state {
            State::Padding => {
                for _ in 0..self.new_columns.len() {
                    line.extend_from_slice(b"| ");
                }
            }
            State::Skip => {
                line.extend_from_slice(b"...");
                let s = if self.needs_pre_commit_line() {
                    State::PreCommit
                } else {
                    State::Commit
                };
                self.update_state(s);
            }
            State::PreCommit => self.pre_commit_line(&mut line),
            State::Commit => {
                self.commit_line(&mut line);
                commit = true;
            }
            State::PostMerge => self.post_merge_line(&mut line),
            State::Collapsing => self.collapsing_line(&mut line),
        }
        self.pad(&mut line);
        (line, commit)
    }

    fn pad(&self, line: &mut Vec<u8>) {
        if line.len() < self.width {
            line.resize(self.width, b' ');
        }
    }

    /// The graph beside the line that separates two commits.
    pub fn padding_line(&mut self) -> Vec<u8> {
        if self.state != State::Commit {
            return self.next_line().0;
        }
        let mut line = Vec::new();
        for i in 0..self.columns.len() {
            line.push(b'|');
            if self.is_commit(&self.columns[i]) && self.num_parents() > 2 {
                line.resize(line.len() + (self.num_parents() - 2) * 2, b' ');
            } else {
                line.push(b' ');
            }
        }
        self.pad(&mut line);
        self.prev_state = State::Padding;
        line
    }

    fn pre_commit_line(&mut self, line: &mut Vec<u8>) {
        let mut seen_this = false;
        for i in 0..self.columns.len() {
            if self.is_commit(&self.columns[i]) {
                seen_this = true;
                line.push(b'|');
                line.resize(line.len() + self.expansion_row, b' ');
            } else if seen_this && self.expansion_row == 0 {
                if self.prev_state == State::PostMerge && self.prev_commit_index < i {
                    line.push(b'\\');
                } else {
                    line.push(b'|');
                }
            } else if seen_this {
                line.push(b'\\');
            } else {
                line.push(b'|');
            }
            line.push(b' ');
        }
        self.expansion_row += 1;
        if !self.needs_pre_commit_line() {
            self.update_state(State::Commit);
        }
    }

    fn commit_line(&mut self, line: &mut Vec<u8>) {
        let mut seen_this = false;
        let num_columns = self.columns.len();
        for i in 0..=num_columns {
            let this = if i == num_columns {
                if seen_this {
                    break;
                }
                true
            } else {
                self.is_commit(&self.columns[i])
            };
            if this {
                seen_this = true;
                line.push(b'*');
                if self.num_parents() > 2 {
                    let dashed = self.num_dashed_parents();
                    for d in 0..dashed {
                        line.push(b'-');
                        line.push(if d == dashed - 1 { b'.' } else { b'-' });
                    }
                }
            } else if seen_this && self.edges_added > 1 {
                line.push(b'\\');
            } else if seen_this && self.edges_added == 1 {
                if self.prev_state == State::PostMerge
                    && self.prev_edges_added > 0
                    && self.prev_commit_index < i
                {
                    line.push(b'\\');
                } else {
                    line.push(b'|');
                }
            } else if self.prev_state == State::Collapsing
                && self.old_mapping.get(2 * i + 1) == Some(&(i as i32))
                && self.mapping.get(2 * i).is_some_and(|&m| m < i as i32)
            {
                line.push(b'/');
            } else {
                line.push(b'|');
            }
            line.push(b' ');
        }
        let s = if self.num_parents() > 1 {
            State::PostMerge
        } else if self.mapping_correct() {
            State::Padding
        } else {
            State::Collapsing
        };
        self.update_state(s);
    }

    fn post_merge_line(&mut self, line: &mut Vec<u8>) {
        let mut seen_this = false;
        let first_parent = self.parents.first().copied();
        let mut parent_col = false;
        let num_columns = self.columns.len();
        for i in 0..=num_columns {
            let col = if i == num_columns {
                if seen_this {
                    break;
                }
                self.commit
            } else {
                Some(self.columns[i])
            };
            if col == self.commit {
                seen_this = true;
                let mut idx = self.merge_layout as usize;
                let n = self.num_parents();
                for j in 0..n {
                    line.push(MERGE_CHARS[idx]);
                    if idx == 2 {
                        if self.edges_added > 0 || j + 1 < n {
                            line.push(b' ');
                        }
                    } else {
                        idx += 1;
                    }
                }
                if self.edges_added == 0 {
                    line.push(b' ');
                }
            } else if seen_this {
                line.push(if self.edges_added > 0 { b'\\' } else { b'|' });
                line.push(b' ');
            } else {
                line.push(b'|');
                if self.merge_layout != 0 || i + 1 != self.commit_index {
                    line.push(if parent_col { b'_' } else { b' ' });
                }
            }
            if col.is_some() && col == first_parent {
                parent_col = true;
            }
        }
        let s = if self.mapping_correct() {
            State::Padding
        } else {
            State::Collapsing
        };
        self.update_state(s);
    }

    fn collapsing_line(&mut self, line: &mut Vec<u8>) {
        let mut used_horizontal = false;
        let mut horizontal_edge: i32 = -1;
        let mut horizontal_edge_target: i32 = -1;
        std::mem::swap(&mut self.mapping, &mut self.old_mapping);
        for m in &mut self.mapping[..self.mapping_size] {
            *m = -1;
        }
        for i in 0..self.mapping_size {
            let target = self.old_mapping[i];
            if target < 0 {
                continue;
            }
            let t2 = target as usize * 2;
            if t2 == i {
                self.mapping[i] = target;
            } else if self.mapping[i - 1] < 0 {
                self.mapping[i - 1] = target;
                if horizontal_edge == -1 {
                    horizontal_edge = i as i32;
                    horizontal_edge_target = target;
                    let mut j = t2 + 3;
                    while j + 2 < i {
                        self.mapping[j] = target;
                        j += 2;
                    }
                }
            } else if self.mapping[i - 1] != target {
                self.mapping[i - 2] = target;
                if horizontal_edge == -1 {
                    horizontal_edge_target = target;
                    horizontal_edge = i as i32 - 1;
                    let mut j = t2 + 3;
                    while j + 2 < i {
                        self.mapping[j] = target;
                        j += 2;
                    }
                }
            }
        }
        let size = self.mapping_size;
        self.old_mapping[..size].copy_from_slice(&self.mapping[..size]);
        if self.mapping[size - 1] < 0 {
            self.mapping_size -= 1;
        }
        for i in 0..self.mapping_size {
            let target = self.mapping[i];
            if target < 0 {
                line.push(b' ');
            } else if target as usize * 2 == i {
                line.push(b'|');
            } else if target == horizontal_edge_target && i as i32 != horizontal_edge - 1 {
                if i != target as usize * 2 + 3 {
                    self.mapping[i] = -1;
                }
                used_horizontal = true;
                line.push(b'_');
            } else {
                if used_horizontal && (i as i32) < horizontal_edge {
                    self.old_mapping[i] = -1;
                }
                line.push(b'/');
            }
        }
        if self.mapping_correct() {
            self.update_state(State::Padding);
        }
    }

    /// git's graph_show_commit: the graph's lines down to the commit's own, which is
    /// left open for the commit's text.
    pub fn show_commit(&mut self, out: &mut Vec<u8>) {
        if self.finished() {
            out.extend_from_slice(&self.padding_line());
            return;
        }
        while !self.finished() {
            let (line, commit) = self.next_line();
            out.extend_from_slice(&line);
            if commit {
                return;
            }
            out.push(b'\n');
        }
    }

    /// git's graph_show_commit_msg: the commit's text, each line after the first behind
    /// the graph, then the graph's lines the commit still needs.
    pub fn show_message(&mut self, out: &mut Vec<u8>, msg: &[u8]) {
        for (n, piece) in msg.split_inclusive(|&b| b == b'\n').enumerate() {
            if n > 0 {
                out.extend_from_slice(&self.next_line().0);
            }
            out.extend_from_slice(piece);
        }
        let terminated = msg.last() == Some(&b'\n');
        if self.finished() {
            return;
        }
        if !terminated {
            out.push(b'\n');
        }
        loop {
            out.extend_from_slice(&self.next_line().0);
            if self.finished() {
                break;
            }
            out.push(b'\n');
        }
        if terminated {
            out.push(b'\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Draws commits given as (id, parents), newest first, as log --graph
    /// --format=%s does.
    fn draw(commits: &[(u8, &[u8])]) -> String {
        let oid = |n: u8| {
            let mut o = Oid::default();
            o.0[0] = n;
            o
        };
        let interesting = commits.iter().map(|c| oid(c.0)).collect();
        let mut graph = Graph::new(interesting, false);
        let mut out = Vec::new();
        for (id, parents) in commits {
            let commit = Commit {
                id: oid(*id),
                tree: Oid::default(),
                parents: parents.iter().map(|p| oid(*p)).collect(),
                author: Vec::new(),
                committer: Vec::new(),
                body: 0,
                data: Default::default(),
            };
            graph.update(&commit);
            graph.show_commit(&mut out);
            graph.show_message(&mut out, &[b'a' + id]);
            out.push(b'\n');
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn draws_merges_as_git_does() {
        assert_eq!(
            draw(&[(3, &[1, 2]), (2, &[1]), (1, &[])]),
            "*   d\n|\\  \n| * c\n|/  \n* b\n"
        );
    }
}
