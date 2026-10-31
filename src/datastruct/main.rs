//! Port of `src/datastruct/main.zig`.
//!
//! STATUS: ported
//!
//! The datastruct package contains data structures or anything closely
//! related to data structures.

// @generated-mods-begin (tools/scaffold.py; do not edit by hand)
#[path = "blocking_queue.rs"]
pub mod blocking_queue;
#[path = "cache_table.rs"]
pub mod cache_table;
#[path = "circ_buf.rs"]
pub mod circ_buf;
#[path = "comparison.rs"]
pub mod comparison;
#[path = "comptime_int_set.rs"]
pub mod comptime_int_set;
#[path = "intrusive_linked_list.rs"]
pub mod intrusive_linked_list;
#[path = "limited_allocator.rs"]
pub mod limited_allocator;
#[path = "message_data.rs"]
pub mod message_data;
#[path = "segmented_list.rs"]
pub mod segmented_list;
#[path = "split_tree.rs"]
pub mod split_tree;
#[path = "untouched_pool.rs"]
pub mod untouched_pool;
#[path = "wasm_page_pool.rs"]
pub mod wasm_page_pool;
// @generated-mods-end

pub use blocking_queue::BlockingQueue;
pub use cache_table::CacheTable;
pub use circ_buf::CircBuf;
pub use comptime_int_set::ComptimeIntSet;
pub use intrusive_linked_list::DoublyLinkedList as IntrusiveDoublyLinkedList;
pub use limited_allocator::LimitedAllocator;
pub use message_data::MessageData;
pub use split_tree::SplitTree;
pub use untouched_pool::UntouchedPool;
#[cfg(any(target_arch = "wasm32", test))]
pub use wasm_page_pool::WasmPagePool;
