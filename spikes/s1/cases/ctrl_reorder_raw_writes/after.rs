pub unsafe fn f(p: *mut u32, q: *mut u32) {
    unsafe {
        *q = 2;
        *p = 1;
    }
}
