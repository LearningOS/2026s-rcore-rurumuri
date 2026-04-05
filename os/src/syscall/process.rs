//! Process management syscalls
use crate::{
    mm::{MapPermission, MemorySet, PageTable, VirtAddr, VirtPageNum},
    task::{
        change_program_brk, current_user_memset, exit_current_and_run_next,
        get_current_task_syscall_count, suspend_current_and_run_next,
    },
};
use core::ptr::write_volatile;

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(_ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    let ms = crate::timer::get_time_ms();
    let timeval = TimeVal {
        sec: ms / 1000,
        usec: (ms % 1000) * 1000,
    };
    let byte_buffer = crate::mm::translated_byte_buffer(
        crate::task::current_user_token(),
        _ts as *const u8,
        core::mem::size_of::<TimeVal>(),
    );
    if byte_buffer.len() == 1 {
        // TimeVal is from one page
        let ptr = byte_buffer[0].as_ptr() as *mut TimeVal;
        unsafe {
            write_volatile(ptr, timeval);
        }
    } else {
        // TimeVal is splitted by two pages
        let ptr1 = byte_buffer[0].as_ptr() as *mut TimeVal;
        let ptr2 = byte_buffer[1].as_ptr() as *mut TimeVal;
        unsafe {
            write_volatile(
                ptr1,
                TimeVal {
                    sec: timeval.sec,
                    usec: 0,
                },
            );
            write_volatile(
                ptr2,
                TimeVal {
                    sec: 0,
                    usec: timeval.usec,
                },
            );
        }
    };
    0
}

/// TODO: Finish sys_trace to pass testcases
/// HINT: You might reimplement it with virtual memory management.
pub fn sys_trace(_trace_request: usize, _id: usize, _data: usize) -> isize {
    trace!("kernel: sys_trace");
    let task_page_table = PageTable::from_token(crate::task::current_user_token());
    match _trace_request {
        // read from _id as *const u8 and return it
        0 => {
            // translate inner will check if the page is valid
            let _id_ppe_o =
                task_page_table.translate(VirtPageNum::from(VirtAddr::from(_id).floor()));
            if _id_ppe_o.is_none() {
                return -1;
            }
            let _id_ppe = _id_ppe_o.unwrap();
            // println!("sys_trace: _id={:?} _data={:?} vpn={:?}", _id, _data, VirtPageNum::from(VirtAddr::from(_id).floor()));
            // println!("PTE: valid={}, readable={}, writable={}, executable={}", _id_ppe.is_valid(), _id_ppe.readable(), _id_ppe.writable(), _id_ppe.executable());
            if !_id_ppe.user() || !_id_ppe.readable() {
                -1
            } else {
                // task_page_table
                //     .translate(VirtPageNum::from(_id))
                //     .unwrap()
                //     .ppn()
                //     .get_bytes_array()[0] as isize
                _id_ppe.ppn().get_bytes_array()[VirtAddr::from(_id).page_offset()] as isize
            }
        }
        // write _data into _id as *mut u8
        1 => {
            // translate inner will check if the page is valid
            let _id_ppe_o =
                task_page_table.translate(VirtPageNum::from(VirtAddr::from(_id).floor()));
            if _id_ppe_o.is_none() {
                return -1;
            }
            let _id_ppe = _id_ppe_o.unwrap();
            // println!("sys_trace: _id={:?} _data={:?} vpn={:?}", _id, _data, VirtPageNum::from(VirtAddr::from(_id).floor()));
            // println!("PTE: valid={}, readable={}, writable={}, executable={}", _id_ppe.is_valid(), _id_ppe.readable(), _id_ppe.writable(), _id_ppe.executable());
            if !_id_ppe.user() || !_id_ppe.writable() {
                -1
            } else {
                unsafe {
                    let ptr = (_id_ppe.ppn().get_bytes_array().as_ptr() as usize
                        + VirtAddr::from(_id).page_offset())
                        as *mut u8;
                    write_volatile(ptr, _data as u8);
                }
                0
            }
        }
        2 => get_current_task_syscall_count(_id) as isize,
        _ => -1,
    }
}

// YOUR JOB: Implement mmap.
pub fn sys_mmap(_start: usize, _len: usize, _port: usize) -> isize {
    trace!("kernel: sys_mmap");
    // println!("sys_mmap: _start={:#x} _len={:#x} _port={:#x}", _start, _len, _port);

    let start_va = VirtAddr::from(_start);
    let end_va = VirtAddr::from(_start + _len);
    // check if _start aligned
    if !start_va.aligned() {
        return -1;
    };
    // check port
    if _port & !0x7 != 0 || _port & 0x7 == 0 {
        return -1;
    };

    // let mut map_permission = MapPermission::empty();
    let mut map_permission = MapPermission::U;
    if _port & 0x1 != 0 {
        map_permission |= MapPermission::R;
    }
    if _port & 0x2 != 0 {
        map_permission |= MapPermission::W;
    }
    if _port & 0x4 != 0 {
        map_permission |= MapPermission::X;
    }
    // let map_area = MapArea::new(va_start, va_end, MapType::Framed, map_permission);
    // map_area.map(PageTable::from_token(crate::task::current_user_token()));
    let user_memset: &mut MemorySet = current_user_memset();

    let start_vpn = VirtPageNum::from(start_va);
    let end_vpn = end_va.ceil();

    for i in start_vpn.0..end_vpn.0 {
        // println!("check vpn: {:x}", i);
        let vpn = VirtPageNum(i);
        let pte_check = user_memset.translate(vpn);

        // ATTENTION: user_memset.translate will return Some(pte) at the 3rd level, even the page is not mapped (invalid)
        // So we need to check if the page is valid before mapping, otherwise kernel will panic and exit in PageTable::map when it finds the page is already mapped
        if pte_check.is_some() && pte_check.unwrap().is_valid() {
            // println!("vpn {} is already mapped", i);
            return -1;
        }
    }
    // insert_framed_area -> push -> MapArea::new will align start_va down and end_va up
    // PageTable::map will check if the page is mapped before mapping, but kernel will panic and exit
    user_memset.insert_framed_area(start_va, end_va, map_permission);
    0
}

// YOUR JOB: Implement munmap.
pub fn sys_munmap(_start: usize, _len: usize) -> isize {
    trace!("kernel: sys_munmap");

    let start_va = VirtAddr::from(_start);
    let end_va = VirtAddr::from(_start + _len);

    // check if _start aligned
    if !start_va.aligned() {
        return -1;
    };

    let user_memset: &mut MemorySet = current_user_memset();

    // check if the pages in the range are mapped and valid before unmapping, otherwise kernel will panic and exit in PageTable::unmap when it finds the
    let start_vpn = VirtPageNum::from(start_va);
    let end_vpn = VirtPageNum::from(end_va.ceil()); // align up end_va
    for i in start_vpn.0..end_vpn.0 {
        let vpn = VirtPageNum(i);
        let pte_check = user_memset.translate(vpn);

        if pte_check.is_none() || !pte_check.unwrap().is_valid() {
            // println!("vpn {} is not mapped or invalid", i);
            return -1;
        }
    }

    let mut task_page_table = PageTable::from_token(crate::task::current_user_token());
    for i in start_vpn.0..end_vpn.0 {
        let vpn = VirtPageNum(i);
        task_page_table.unmap(vpn);
    }
    0
}
/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}
