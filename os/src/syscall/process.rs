//! Process management syscalls
//!
use core::ptr::write_volatile;

use alloc::sync::Arc;

use crate::{
    fs::{OpenFlags, open_file}, mm::{MapPermission, MemorySet, PageTable, VirtAddr, VirtPageNum, translated_refmut, translated_str}, task::{
        add_task, current_task, current_user_token, exit_current_and_run_next,
        suspend_current_and_run_next,
    }
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

pub fn sys_yield() -> isize {
    //trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

pub fn sys_getpid() -> isize {
    trace!("kernel: sys_getpid pid:{}", current_task().unwrap().pid.0);
    current_task().unwrap().pid.0 as isize
}

pub fn sys_fork() -> isize {
    trace!("kernel:pid[{}] sys_fork", current_task().unwrap().pid.0);
    let current_task = current_task().unwrap();
    let new_task = current_task.fork();
    let new_pid = new_task.pid.0;
    // modify trap context of new_task, because it returns immediately after switching
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.x[10] = 0;
    // add new task to scheduler
    add_task(new_task);
    new_pid as isize
}

pub fn sys_exec(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_exec", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let task = current_task().unwrap();
        task.exec(all_data.as_slice());
        0
    } else {
        -1
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    //trace!("kernel: sys_waitpid");
    let task = current_task().unwrap();
    // find a child process

    // ---- access current PCB exclusively
    let mut inner = task.inner_exclusive_access();
    if !inner
        .children
        .iter()
        .any(|p| pid == -1 || pid as usize == p.getpid())
    {
        return -1;
        // ---- release current PCB
    }
    let pair = inner.children.iter().enumerate().find(|(_, p)| {
        // ++++ temporarily access child PCB exclusively
        p.inner_exclusive_access().is_zombie() && (pid == -1 || pid as usize == p.getpid())
        // ++++ release child PCB
    });
    if let Some((idx, _)) = pair {
        let child = inner.children.remove(idx);
        // confirm that child will be deallocated after being removed from children list
        assert_eq!(Arc::strong_count(&child), 1);
        let found_pid = child.getpid();
        // ++++ temporarily access child PCB exclusively
        let exit_code = child.inner_exclusive_access().exit_code;
        // ++++ release child PCB
        *translated_refmut(inner.memory_set.token(), exit_code_ptr) = exit_code;
        found_pid as isize
    } else {
        -2
    }
    // ---- release current PCB automatically
}


/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(_ts: *mut TimeVal, _tz: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_get_time",
        current_task().unwrap().pid.0
    );
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

/// YOUR JOB: Implement mmap.
pub fn sys_mmap(_start: usize, _len: usize, _port: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_mmap",
        current_task().unwrap().pid.0
    );
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
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    let user_memset: &mut MemorySet = &mut inner.memory_set;

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

/// YOUR JOB: Implement munmap.
pub fn sys_munmap(_start: usize, _len: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_munmap",
        current_task().unwrap().pid.0
    );

    let start_va = VirtAddr::from(_start);
    let end_va = VirtAddr::from(_start + _len);

    // check if _start aligned
    if !start_va.aligned() {
        return -1;
    };

    let token = current_user_token();
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    let user_memset: &mut MemorySet = &mut inner.memory_set;

    // check if the pages in the range are mapped and valid before unmapping, otherwise kernel will panic and exit in PageTable::unmap when it finds the
    let start_vpn = VirtPageNum::from(start_va);
    let end_vpn = end_va.ceil(); // align up end_va
    for i in start_vpn.0..end_vpn.0 {
        let vpn = VirtPageNum(i);
        let pte_check = user_memset.translate(vpn);

        if pte_check.is_none() || !pte_check.unwrap().is_valid() {
            return -1;
        }
    }

    let mut task_page_table = PageTable::from_token(token);
    for i in start_vpn.0..end_vpn.0 {
        let vpn = VirtPageNum(i);
        task_page_table.unmap(vpn);
    }
    0
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel:pid[{}] sys_sbrk", current_task().unwrap().pid.0);
    if let Some(old_brk) = current_task().unwrap().change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

/// YOUR JOB: Implement spawn.
/// HINT: fork + exec =/= spawn
pub fn sys_spawn(_path: *const u8) -> isize {
    trace!(
        "kernel:pid[{}] sys_spawn",
        current_task().unwrap().pid.0
    );
    let current_task = current_task().unwrap();
    let new_task = current_task.fork();
    let new_pid = new_task.pid.0;

    let token = current_user_token();
    let path = translated_str(token, _path);
    // if let Some(data) = get_app_data_by_name(path.as_str()) {
    //     new_task.exec(data);
    // } else {
    //     return -1
    // }
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        new_task.exec(all_data.as_slice());
        add_task(new_task);
        new_pid as isize
    } else {
        -1
    }
}

// YOUR JOB: Set task priority.
pub fn sys_set_priority(_prio: isize) -> isize {
    trace!(
        "kernel:pid[{}] sys_set_priority NOT IMPLEMENTED",
        current_task().unwrap().pid.0
    );
    if _prio >= 2 {
        _prio
    } else {
        -1
    }
}