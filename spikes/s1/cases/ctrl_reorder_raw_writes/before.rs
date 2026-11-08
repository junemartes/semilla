pub unsafe fn f(p: *mut u32, q: *mut u32) {
    unsafe {
        *p = 1;
        *q = 2;
    }
}
