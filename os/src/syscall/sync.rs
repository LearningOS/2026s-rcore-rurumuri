use crate::sync::{Condvar, Mutex, MutexBlocking, MutexSpin, Semaphore};
use crate::task::{block_current_and_run_next, current_process, current_task};
use crate::timer::{add_timer, get_time_ms};
use alloc::sync::Arc;
/// sleep syscall
pub fn sys_sleep(ms: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_sleep",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let expire_ms = get_time_ms() + ms;
    let task = current_task().unwrap();
    add_timer(expire_ms, task);
    block_current_and_run_next();
    0
}
/// mutex create syscall
pub fn sys_mutex_create(blocking: bool) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mutex: Option<Arc<dyn Mutex>> = if !blocking {
        Some(Arc::new(MutexSpin::new()))
    } else {
        Some(Arc::new(MutexBlocking::new()))
    };
    let mut process_inner = process.inner_exclusive_access();
    let id = if let Some(id) = process_inner
        .mutex_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.mutex_list[id] = mutex;
        id
    } else {
        process_inner.mutex_list.push(mutex);
        process_inner.mutex_list.len() - 1
    };
    process_inner.ensure_deadlock_matrices();
    id as isize
}
/// mutex lock syscall
pub fn sys_mutex_lock(mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_lock",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let task = current_task().unwrap();
    let process = task.process.upgrade().unwrap();
    let tid = {
        let task_inner = task.inner_exclusive_access();
        task_inner.res.as_ref().unwrap().tid
    };
    let process_inner = process.inner_exclusive_access();
    if mutex_id >= process_inner.mutex_list.len() || process_inner.mutex_list[mutex_id].is_none() {
        return -1;
    }
    let mutex = Arc::clone(process_inner.mutex_list[mutex_id].as_ref().unwrap());
    let detect_enabled = process_inner.deadlock_detect;
    drop(process_inner);
    if detect_enabled {
        let process_inner = process.inner_exclusive_access();
        if process_inner.check_mutex_deadlock(tid, mutex_id) {
            return -0xdead as isize;
        }
    }
    mutex.lock();
    if detect_enabled {
        let mut process_inner = process.inner_exclusive_access();
        if tid < process_inner.mutex_allocation.len() && mutex_id < process_inner.mutex_allocation[tid].len() {
            process_inner.mutex_allocation[tid][mutex_id] = 1;
        }
    }
    0
}
/// mutex unlock syscall
pub fn sys_mutex_unlock(mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_unlock",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let task = current_task().unwrap();
    let process = task.process.upgrade().unwrap();
    let tid = {
        let task_inner = task.inner_exclusive_access();
        task_inner.res.as_ref().unwrap().tid
    };
    let process_inner = process.inner_exclusive_access();
    if mutex_id >= process_inner.mutex_list.len() || process_inner.mutex_list[mutex_id].is_none() {
        return -1;
    }
    let mutex = Arc::clone(process_inner.mutex_list[mutex_id].as_ref().unwrap());
    let detect_enabled = process_inner.deadlock_detect;
    drop(process_inner);
    mutex.unlock();
    if detect_enabled {
        let mut process_inner = process.inner_exclusive_access();
        if tid < process_inner.mutex_allocation.len() && mutex_id < process_inner.mutex_allocation[tid].len() {
            process_inner.mutex_allocation[tid][mutex_id] = 0;
        }
    }
    0
}
/// semaphore create syscall
pub fn sys_semaphore_create(res_count: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    let id = if let Some(id) = process_inner
        .semaphore_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.semaphore_list[id] = Some(Arc::new(Semaphore::new(res_count)));
        id
    } else {
        process_inner
            .semaphore_list
            .push(Some(Arc::new(Semaphore::new(res_count))));
        process_inner.semaphore_list.len() - 1
    };
    process_inner.ensure_deadlock_matrices();
    id as isize
}
/// semaphore up syscall
pub fn sys_semaphore_up(sem_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_up",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let task = current_task().unwrap();
    let process = task.process.upgrade().unwrap();
    let tid = {
        let task_inner = task.inner_exclusive_access();
        task_inner.res.as_ref().unwrap().tid
    };
    let process_inner = process.inner_exclusive_access();
    if sem_id >= process_inner.semaphore_list.len() || process_inner.semaphore_list[sem_id].is_none() {
        return -1;
    }
    let sem = Arc::clone(process_inner.semaphore_list[sem_id].as_ref().unwrap());
    let detect_enabled = process_inner.deadlock_detect;
    drop(process_inner);
    sem.up();
    if detect_enabled {
        let mut process_inner = process.inner_exclusive_access();
        if tid < process_inner.semaphore_allocation.len() && sem_id < process_inner.semaphore_allocation[tid].len() {
            let alloc = &mut process_inner.semaphore_allocation[tid][sem_id];
            *alloc = (*alloc - 1).max(0isize);
        }
    }
    0
}
/// semaphore down syscall
pub fn sys_semaphore_down(sem_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_down",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let task = current_task().unwrap();
    let process = task.process.upgrade().unwrap();
    let tid = {
        let task_inner = task.inner_exclusive_access();
        task_inner.res.as_ref().unwrap().tid
    };
    let process_inner = process.inner_exclusive_access();
    if sem_id >= process_inner.semaphore_list.len() || process_inner.semaphore_list[sem_id].is_none() {
        return -1;
    }
    let sem = Arc::clone(process_inner.semaphore_list[sem_id].as_ref().unwrap());
    let detect_enabled = process_inner.deadlock_detect;
    drop(process_inner);
    if detect_enabled {
        let process_inner = process.inner_exclusive_access();
        if process_inner.check_sem_deadlock(tid, sem_id) {
            return -0xdead as isize;
        }
    }
    sem.down();
    if detect_enabled {
        let mut process_inner = process.inner_exclusive_access();
        if tid < process_inner.semaphore_allocation.len() && sem_id < process_inner.semaphore_allocation[tid].len() {
            process_inner.semaphore_allocation[tid][sem_id] += 1;
        }
    }
    0
}
/// condvar create syscall
pub fn sys_condvar_create() -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    let id = if let Some(id) = process_inner
        .condvar_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.condvar_list[id] = Some(Arc::new(Condvar::new()));
        id
    } else {
        process_inner
            .condvar_list
            .push(Some(Arc::new(Condvar::new())));
        process_inner.condvar_list.len() - 1
    };
    id as isize
}
/// condvar signal syscall
pub fn sys_condvar_signal(condvar_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_signal",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    let condvar = Arc::clone(process_inner.condvar_list[condvar_id].as_ref().unwrap());
    drop(process_inner);
    condvar.signal();
    0
}
/// condvar wait syscall
pub fn sys_condvar_wait(condvar_id: usize, mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_wait",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    let condvar = Arc::clone(process_inner.condvar_list[condvar_id].as_ref().unwrap());
    let mutex = Arc::clone(process_inner.mutex_list[mutex_id].as_ref().unwrap());
    drop(process_inner);
    condvar.wait(mutex);
    0
}
/// enable deadlock detection syscall
pub fn sys_enable_deadlock_detect(enabled: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_enable_deadlock_detect({})",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid,
        enabled
    );
    if enabled > 1 {
        return -1;
    }
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    process_inner.enable_deadlock_detect(enabled == 1);
    0
}
