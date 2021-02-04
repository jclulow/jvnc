use std::alloc::{Layout, alloc_zeroed, dealloc};

pub struct Framebuffer {
    layout: Layout,
    region: *mut u8,
    pixelsize: usize,
    height: usize,
    width: usize,
}

unsafe impl Send for Framebuffer {}
unsafe impl Sync for Framebuffer {}

impl Framebuffer {
    pub fn new(width: usize, height: usize) -> Self {
        let pixelsize = 4;
        let ncells = width.checked_mul(height).unwrap();
        let size = ncells.checked_mul(pixelsize).unwrap();

        let layout = Layout::from_size_align(size, pixelsize).unwrap();
        let region = unsafe { alloc_zeroed(layout) };
        println!("framebuffer memory @ {:?}", region);

        Framebuffer {
            layout,
            pixelsize,
            region,
            height,
            width,
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn put(&self, x: usize, y: usize, red: u8, green: u8, blue: u8) {
        if x >= self.width || y >= self.height {
            return;
        }

        let mut pix = 0u32;
        pix |= (red as u32) << 16;
        pix |= (green as u32) << 8;
        pix |= blue as u32;

        let pixregion = self.region as *mut u32;
        let target = (y * self.width + x) as isize;

        unsafe { pixregion.offset(target).write_volatile(pix) };
    }

    pub fn get(&self, x: usize, y: usize) -> (u8, u8, u8) {
        if x >= self.width || y >= self.height {
            panic!("out of bounds");
        }

        let pixregion = self.region as *mut u32;
        let target = (y * self.width + x) as isize;

        let pix = unsafe { pixregion.offset(target).read_volatile() };
        ((pix >> 16) as u8, (pix >> 8) as u8, pix as u8)
    }

    #[allow(dead_code)]
    pub fn copy_all(&self) -> Vec<u8> {
        let ncells = self.width.checked_mul(self.height).unwrap();
        let size = ncells.checked_mul(self.pixelsize).unwrap();
        let slice = unsafe { std::slice::from_raw_parts(self.region, size) };
        slice.to_vec()
    }
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        unsafe { dealloc(self.region, self.layout) };
    }
}
