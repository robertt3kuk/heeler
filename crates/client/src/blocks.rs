//! The client-side model of one terminal's blocks (protocol 0.5, capability `terminal.blocks`):
//! what an initial `terminal.blocks` fetch and the `terminal.block` events build up. Pure data
//! apart from [`BlockList::fetch`].

use std::collections::VecDeque;

use staffy_core::TerminalId;
use staffy_protocol::blocks::{BLOCKS_MAX_LIMIT, Block, TerminalBlocksResult};

use crate::client::Client;
use crate::error::ClientError;

/// Blocks kept; the daemon keeps at most this many too.
const MAX_BLOCKS: usize = 2_000;

/// The blocks of one terminal, oldest first, with ascending ids.
///
/// Keep it current by folding [`Notice::TerminalBlock`](crate::Notice::TerminalBlock) into it
/// with [`apply`](Self::apply); after a reconnect, a lagging notice receiver or a
/// restarted terminal, [`fetch`](Self::fetch) a fresh list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockList {
    blocks: VecDeque<Block>,
    viewport_top: u64,
}

impl BlockList {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads every block the daemon keeps, newest page first. Fails with the daemon's
    /// `bad_request` when it does not know `terminal.blocks` (check
    /// [`Client::supports_blocks`](crate::Client::supports_blocks) first).
    pub async fn fetch(client: &Client, terminal: TerminalId) -> Result<Self, ClientError> {
        let mut list = Self::new();
        let mut before = None;
        loop {
            let page = client
                .terminal_blocks(terminal, before, BLOCKS_MAX_LIMIT)
                .await?;
            list.viewport_top = list.viewport_top.max(page.viewport_top);
            let Some(oldest) = page.blocks.last().map(|block| block.id) else {
                break;
            };
            before = Some(oldest);
            // Newest first: the page goes in front of what is already there.
            for block in page.blocks {
                list.blocks.push_front(block);
            }
            if list.blocks.len() >= MAX_BLOCKS {
                break;
            }
        }
        list.blocks.keep_newest(MAX_BLOCKS);
        Ok(list)
    }

    /// Replaces the list with one `terminal.blocks` answer (newest first).
    pub fn replace(&mut self, result: TerminalBlocksResult) {
        self.viewport_top = result.viewport_top;
        self.blocks = result.blocks.into_iter().rev().collect();
        self.blocks.keep_newest(MAX_BLOCKS);
    }

    /// Folds one `terminal.block` event: a known block is replaced, a new one is added (events
    /// arrive in order, so a new block has the highest id), the oldest blocks fall off the end
    /// at the daemon's bound.
    pub fn apply(&mut self, block: Block) {
        match self
            .blocks
            .binary_search_by_key(&block.id, |known| known.id)
        {
            Ok(at) => self.blocks[at] = block,
            Err(at) => {
                self.blocks.insert(at, block);
                self.blocks.keep_newest(MAX_BLOCKS);
            }
        }
    }

    /// The block with `id`.
    pub fn get(&self, id: u64) -> Option<&Block> {
        self.blocks
            .binary_search_by_key(&id, |block| block.id)
            .ok()
            .map(|at| &self.blocks[at])
    }

    /// Oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Block> + ExactSizeIterator {
        self.blocks.iter()
    }

    /// The newest block (the open prompt, or the command that runs).
    pub fn last(&self) -> Option<&Block> {
        self.blocks.back()
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Absolute line number of the viewport's first row when the list was last fetched or
    /// replaced (`terminal.blocks`' `viewport_top`).
    pub fn viewport_top(&self) -> u64 {
        self.viewport_top
    }
}

trait TruncateFront {
    /// Keeps the newest `len` entries.
    fn keep_newest(&mut self, len: usize);
}

impl TruncateFront for VecDeque<Block> {
    fn keep_newest(&mut self, len: usize) {
        while self.len() > len {
            self.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use staffy_protocol::blocks::BlockState;

    use super::*;

    fn block(id: u64, state: BlockState) -> Block {
        Block {
            id,
            prompt_line: id * 3,
            command_line: None,
            output_start: None,
            end_line: None,
            command: None,
            exit_code: None,
            started_at_ms: 1,
            finished_at_ms: None,
            cwd: None,
            state,
        }
    }

    #[test]
    fn events_add_new_blocks_and_replace_known_ones() {
        let mut list = BlockList::new();
        list.apply(block(1, BlockState::Prompt));
        list.apply(block(1, BlockState::Running));
        list.apply(block(2, BlockState::Prompt));
        assert_eq!(list.len(), 2);
        assert_eq!(list.get(1).map(|b| b.state), Some(BlockState::Running));
        assert_eq!(list.last().map(|b| b.id), Some(2));
        assert_eq!(list.iter().map(|b| b.id).collect::<Vec<_>>(), [1, 2]);
    }

    #[test]
    fn a_fetched_page_is_oldest_first_and_events_keep_the_order() {
        let mut list = BlockList::new();
        list.replace(TerminalBlocksResult {
            blocks: vec![
                block(5, BlockState::Finished),
                block(4, BlockState::Finished),
            ],
            viewport_top: 77,
        });
        assert_eq!(list.viewport_top(), 77);
        assert_eq!(list.iter().map(|b| b.id).collect::<Vec<_>>(), [4, 5]);
        list.apply(block(6, BlockState::Prompt));
        // A late event for an older block replaces it in place.
        list.apply(block(4, BlockState::Truncated));
        assert_eq!(list.get(4).map(|b| b.state), Some(BlockState::Truncated));
        assert_eq!(list.iter().map(|b| b.id).collect::<Vec<_>>(), [4, 5, 6]);
    }

    #[test]
    fn the_list_is_bounded_like_the_daemon() {
        let mut list = BlockList::new();
        for id in 1..=(MAX_BLOCKS as u64 + 10) {
            list.apply(block(id, BlockState::Finished));
        }
        assert_eq!(list.len(), MAX_BLOCKS);
        assert_eq!(list.iter().next().map(|b| b.id), Some(11));
        assert!(list.get(5).is_none());
    }
}
