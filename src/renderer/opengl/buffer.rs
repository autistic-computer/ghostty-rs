//! Port of `src/renderer/opengl/buffer.zig`.
//!
//! STATUS: ported

use std::marker::PhantomData;

use crate::pkg::opengl as gl;

/// Options for initializing a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub target: gl::buffer::Target,
    pub usage: gl::buffer::Usage,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            target: gl::buffer::Target::Array,
            usage: gl::buffer::Usage::DynamicDraw,
        }
    }
}

/// OpenGL data storage for a certain set of equal types. This is usually
/// used for vertex buffers, etc. This helpful wrapper makes it easy to
/// prealloc, shrink, grow, sync, buffers with OpenGL.
///
/// `T` must be plain data (`Copy`, no padding requirements beyond its
/// layout); its bytes are uploaded verbatim.
#[derive(Debug)]
pub struct Buffer<T: Copy> {
    /// Underlying `gl.Buffer` instance.
    pub buffer: gl::Buffer,

    /// Options this buffer was allocated with.
    pub opts: Options,

    /// Current allocated length of the data store.
    /// Note this is the number of `T`s, not the size in bytes.
    pub len: usize,

    _marker: PhantomData<T>,
}

impl<T: Copy> Clone for Buffer<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Copy> Copy for Buffer<T> {}

impl<T: Copy> Buffer<T> {
    /// Initialize a buffer with the given length pre-allocated.
    pub fn init(opts: Options, len: usize) -> Result<Self, gl::errors::Error> {
        let buffer = gl::Buffer::create()?;

        let r = (|| {
            let binding = buffer.bind(opts.target)?;
            let r = binding.set_data_null_manual(len * std::mem::size_of::<T>(), opts.usage);
            binding.unbind();
            r
        })();
        if let Err(err) = r {
            // errdefer buffer.destroy();
            buffer.destroy();
            return Err(err);
        }

        Ok(Buffer {
            buffer,
            opts,
            len,
            _marker: PhantomData,
        })
    }

    /// Init the buffer filled with the given data.
    pub fn init_fill(opts: Options, data: &[T]) -> Result<Self, gl::errors::Error> {
        let buffer = gl::Buffer::create()?;

        let r = (|| {
            let binding = buffer.bind(opts.target)?;
            let r = binding.set_data(data, opts.usage);
            binding.unbind();
            r
        })();
        if let Err(err) = r {
            // errdefer buffer.destroy();
            buffer.destroy();
            return Err(err);
        }

        Ok(Buffer {
            buffer,
            opts,
            // NOTE: this is the size in bytes, not the number of `T`s,
            // exactly as in the Zig code.
            len: std::mem::size_of_val(data),
            _marker: PhantomData,
        })
    }

    pub fn deinit(self) {
        self.buffer.destroy();
    }

    /// Sync new contents to the buffer. The data is expected to be the
    /// complete contents of the buffer. If the amount of data is larger
    /// than the buffer length, the buffer will be reallocated.
    ///
    /// If the amount of data is smaller than the buffer length, the
    /// remaining data in the buffer is left untouched.
    pub fn sync(&mut self, data: &[T]) -> Result<(), gl::errors::Error> {
        let binding = self.buffer.bind(self.opts.target)?;
        let r = (|| {
            // If we need more space than our buffer has, we need to reallocate.
            if data.len() > self.len {
                // Reallocate the buffer to hold double what we require.
                self.len = data.len() * 2;
                binding
                    .set_data_null_manual(self.len * std::mem::size_of::<T>(), self.opts.usage)?;
            }

            // We can fit within the buffer so we can just replace bytes.
            binding.set_sub_data(0, data)
        })();
        binding.unbind();
        r
    }

    /// Like Buffer.sync but takes data from an array of ArrayLists,
    /// rather than a single array. Returns the number of items synced.
    pub fn sync_from_array_lists(&mut self, lists: &[Vec<T>]) -> Result<usize, gl::errors::Error> {
        let binding = self.buffer.bind(self.opts.target)?;
        let r = (|| {
            let mut total_len: usize = 0;
            for list in lists {
                total_len += list.len();
            }

            // If we need more space than our buffer has, we need to reallocate.
            if total_len > self.len {
                // Reallocate the buffer to hold double what we require.
                self.len = total_len * 2;
                binding
                    .set_data_null_manual(self.len * std::mem::size_of::<T>(), self.opts.usage)?;
            }

            // We can fit within the buffer so we can just replace bytes.
            let mut i: usize = 0;

            for list in lists {
                binding.set_sub_data(i, list.as_slice())?;
                i += list.len() * std::mem::size_of::<T>();
            }

            Ok(total_len)
        })();
        binding.unbind();
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::opengl::device::test_context::TestContext;

    /// Read back `n` u32s from the start of the buffer.
    fn read_back(buf: &Buffer<u32>, n: usize) -> Vec<u32> {
        let mut out = vec![0u32; n];
        let binding = buf.buffer.bind(buf.opts.target).unwrap();
        gl::glad::call!(GetBufferSubData(
            buf.opts.target.0,
            0,
            (n * 4) as gl::c::GLsizeiptr,
            out.as_mut_ptr() as *mut std::os::raw::c_void,
        ));
        binding.unbind();
        gl::errors::get_error().unwrap();
        out
    }

    // Not a Zig test: buffer.zig has none.
    #[test]
    fn sync_grows_and_writes() {
        let _ctx = TestContext::new();

        let mut buf = Buffer::<u32>::init(Options::default(), 2).unwrap();
        assert_eq!(2, buf.len);
        buf.sync(&[1, 2]).unwrap();
        assert_eq!(2, buf.len);
        buf.sync(&[1, 2, 3]).unwrap();
        assert_eq!(6, buf.len);
        assert_eq!(vec![1, 2, 3], read_back(&buf, 3));

        let n = buf
            .sync_from_array_lists(&[vec![4, 5], vec![], vec![6, 7, 8, 9, 10]])
            .unwrap();
        assert_eq!(7, n);
        assert_eq!(14, buf.len);
        assert_eq!(vec![4, 5, 6, 7, 8, 9, 10], read_back(&buf, 7));
        buf.deinit();

        let buf = Buffer::<u32>::init_fill(Options::default(), &[7, 8, 9]).unwrap();
        // Zig quirk: the length is in bytes for initFill.
        assert_eq!(12, buf.len);
        assert_eq!(vec![7, 8, 9], read_back(&buf, 3));
        buf.deinit();
    }
}
