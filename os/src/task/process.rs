//! Implementation of  [`ProcessControlBlock`]

use super::id::RecycleAllocator;
use super::manager::insert_into_pid2process;
use super::TaskControlBlock;
use super::{add_task, SignalFlags};
use super::{pid_alloc, PidHandle};
use crate::fs::{File, Stdin, Stdout};
use crate::mm::{translated_refmut, MemorySet, KERNEL_SPACE};
use crate::sync::{Condvar, Mutex, Semaphore, UPSafeCell};
use crate::trap::{trap_handler, TrapContext};
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefMut;

/// Process Control Block
pub struct ProcessControlBlock {
    /// immutable
    pub pid: PidHandle,
    /// mutable
    inner: UPSafeCell<ProcessControlBlockInner>,
}

/// Inner of Process Control Block
pub struct ProcessControlBlockInner {
    /// is zombie?
    pub is_zombie: bool,
    /// memory set(address space)
    pub memory_set: MemorySet,
    /// parent process
    pub parent: Option<Weak<ProcessControlBlock>>,
    /// children process
    pub children: Vec<Arc<ProcessControlBlock>>,
    /// exit code
    pub exit_code: i32,
    /// file descriptor table
    pub fd_table: Vec<Option<Arc<dyn File + Send + Sync>>>,
    /// signal flags
    pub signals: SignalFlags,
    /// tasks(also known as threads)
    pub tasks: Vec<Option<Arc<TaskControlBlock>>>,
    /// task resource allocator
    pub task_res_allocator: RecycleAllocator,
    /// mutex list
    pub mutex_list: Vec<Option<Arc<dyn Mutex>>>,
    /// semaphore list
    pub semaphore_list: Vec<Option<Arc<Semaphore>>>,
    /// condvar list
    pub condvar_list: Vec<Option<Arc<Condvar>>>,
    /// deadlock detection enabled
    pub deadlock_detect: bool,
    /// semaphore allocation matrix: semaphore_allocation[tid][sem_id]
    pub semaphore_allocation: Vec<Vec<isize>>,
    /// mutex allocation matrix: mutex_allocation[tid][mutex_id] (0 or 1)
    pub mutex_allocation: Vec<Vec<isize>>,
}

impl ProcessControlBlockInner {
    #[allow(unused)]
    /// get the address of app's page table
    pub fn get_user_token(&self) -> usize {
        self.memory_set.token()
    }
    /// allocate a new file descriptor
    pub fn alloc_fd(&mut self) -> usize {
        if let Some(fd) = (0..self.fd_table.len()).find(|fd| self.fd_table[*fd].is_none()) {
            fd
        } else {
            self.fd_table.push(None);
            self.fd_table.len() - 1
        }
    }
    /// allocate a new task id
    pub fn alloc_tid(&mut self) -> usize {
        self.task_res_allocator.alloc()
    }
    /// deallocate a task id
    pub fn dealloc_tid(&mut self, tid: usize) {
        self.task_res_allocator.dealloc(tid);
        if tid < self.semaphore_allocation.len() {
            if let Some(row) = self.semaphore_allocation.get_mut(tid) {
                for v in row.iter_mut() {
                    *v = 0;
                }
            }
        }
        if tid < self.mutex_allocation.len() {
            if let Some(row) = self.mutex_allocation.get_mut(tid) {
                for v in row.iter_mut() {
                    *v = 0;
                }
            }
        }
    }
    /// the count of tasks(threads) in this process
    pub fn thread_count(&self) -> usize {
        self.tasks.len()
    }
    /// get a task with tid in this process
    pub fn get_task(&self, tid: usize) -> Arc<TaskControlBlock> {
        self.tasks[tid].as_ref().unwrap().clone()
    }

    pub fn ensure_deadlock_matrices(&mut self) {
        let n = self.tasks.len();
        let m_sem = self.semaphore_list.len();
        let m_mutex = self.mutex_list.len();
        // resize semaphore allocation
        while self.semaphore_allocation.len() < n {
            self.semaphore_allocation.push(vec![0isize; m_sem]);
        }
        for row in &mut self.semaphore_allocation {
            row.resize(m_sem, 0);
        }
        // resize mutex allocation
        while self.mutex_allocation.len() < n {
            self.mutex_allocation.push(vec![0isize; m_mutex]);
        }
        for row in &mut self.mutex_allocation {
            row.resize(m_mutex, 0);
        }
    }

    pub fn enable_deadlock_detect(&mut self, enabled: bool) {
        self.deadlock_detect = enabled;
        self.ensure_deadlock_matrices();
    }

    fn is_safe(
        available: Vec<isize>,
        allocation: &[Vec<isize>],
        need: &[Vec<isize>],
        n: usize,
        m: usize,
    ) -> bool {
        let mut work = available;
        let mut finish = vec![false; n];
        let mut progress = true;
        while progress {
            progress = false;
            for i in 0..n {
                if !finish[i] {
                    let mut can = true;
                    for j in 0..m {
                        if j >= need[i].len() || need[i][j] > work[j] {
                            can = false;
                            break;
                        }
                    }
                    if can {
                        for j in 0..m {
                            if j < allocation[i].len() {
                                work[j] += allocation[i][j];
                            }
                        }
                        finish[i] = true;
                        progress = true;
                    }
                }
            }
        }
        finish.iter().all(|&f| f)
    }

    pub fn check_sem_deadlock(&self, tid: usize, sem_id: usize) -> bool {
        let n = self.tasks.len();
        let m = self.semaphore_list.len();
        if tid >= n || sem_id >= m || self.semaphore_list[sem_id].is_none() {
            return false;
        }
        let mut available = vec![0isize; m];
        let mut need = vec![vec![0isize; m]; n];
        let allocation = self.semaphore_allocation.clone();
        for (j, sem_opt) in self.semaphore_list.iter().enumerate() {
            if let Some(sem) = sem_opt.as_ref() {
                let wait_queue = {
                    let sem_inner = sem.inner.exclusive_access();
                    available[j] = sem_inner.count.max(0isize);
                    sem_inner.wait_queue.iter().cloned().collect::<Vec<_>>()
                };
                for task in wait_queue {
                    let task_inner = task.inner_exclusive_access();
                    if let Some(res) = &task_inner.res {
                        let wtid = res.tid;
                        if wtid < n {
                            need[wtid][j] = 1;
                        }
                    }
                }
            }
        }
        need[tid][sem_id] = 1;
        let safe = Self::is_safe(available, &allocation, &need, n, m);
        !safe
    }

    pub fn check_mutex_deadlock(&self, tid: usize, mutex_id: usize) -> bool {
        let n = self.tasks.len();
        let m = self.mutex_list.len();
        if tid >= n || mutex_id >= m || self.mutex_list[mutex_id].is_none() {
            return false;
        }
        let mut available = vec![0isize; m];
        let mut need = vec![vec![0isize; m]; n];
        let allocation = self.mutex_allocation.clone();
        for (j, mutex_opt) in self.mutex_list.iter().enumerate() {
            if let Some(mutex) = mutex_opt.as_ref() {
                available[j] = if mutex.is_locked() { 0 } else { 1 };
                let wait_queue = mutex.get_wait_queue();
                for task in wait_queue {
                    let task_inner = task.inner_exclusive_access();
                    if let Some(res) = &task_inner.res {
                        let wtid = res.tid;
                        if wtid < n {
                            need[wtid][j] = 1;
                        }
                    }
                }
            }
        }
        need[tid][mutex_id] = 1;
        let safe = Self::is_safe(available, &allocation, &need, n, m);
        !safe
    }
}

impl ProcessControlBlock {
    /// inner_exclusive_access
    pub fn inner_exclusive_access(&self) -> RefMut<'_, ProcessControlBlockInner> {
        self.inner.exclusive_access()
    }
    /// new process from elf file
    pub fn new(elf_data: &[u8]) -> Arc<Self> {
        trace!("kernel: ProcessControlBlock::new");
        // memory_set with elf program headers/trampoline/trap context/user stack
        let (memory_set, ustack_base, entry_point) = MemorySet::from_elf(elf_data);
        // allocate a pid
        let pid_handle = pid_alloc();
        let process = Arc::new(Self {
            pid: pid_handle,
            inner: unsafe {
                UPSafeCell::new(ProcessControlBlockInner {
                    is_zombie: false,
                    memory_set,
                    parent: None,
                    children: Vec::new(),
                    exit_code: 0,
                    fd_table: vec![
                        // 0 -> stdin
                        Some(Arc::new(Stdin)),
                        // 1 -> stdout
                        Some(Arc::new(Stdout)),
                        // 2 -> stderr
                        Some(Arc::new(Stdout)),
                    ],
                    signals: SignalFlags::empty(),
                    tasks: Vec::new(),
                    task_res_allocator: RecycleAllocator::new(),
                    mutex_list: Vec::new(),
                    semaphore_list: Vec::new(),
                    condvar_list: Vec::new(),
                    deadlock_detect: false,
                    semaphore_allocation: Vec::new(),
                    mutex_allocation: Vec::new(),
                })
            },
        });
        // create a main thread, we should allocate ustack and trap_cx here
        let task = Arc::new(TaskControlBlock::new(
            Arc::clone(&process),
            ustack_base,
            true,
        ));
        // prepare trap_cx of main thread
        let task_inner = task.inner_exclusive_access();
        let trap_cx = task_inner.get_trap_cx();
        let ustack_top = task_inner.res.as_ref().unwrap().ustack_top();
        let kstack_top = task.kstack.get_top();
        drop(task_inner);
        *trap_cx = TrapContext::app_init_context(
            entry_point,
            ustack_top,
            KERNEL_SPACE.exclusive_access().token(),
            kstack_top,
            trap_handler as usize,
        );
        // add main thread to the process
        let mut process_inner = process.inner_exclusive_access();
        process_inner.tasks.push(Some(Arc::clone(&task)));
        process_inner.ensure_deadlock_matrices();
        drop(process_inner);
        insert_into_pid2process(process.getpid(), Arc::clone(&process));
        // add main thread to scheduler
        add_task(task);
        process
    }

    /// Only support processes with a single thread.
    pub fn exec(self: &Arc<Self>, elf_data: &[u8], args: Vec<String>) {
        trace!("kernel: exec");
        assert_eq!(self.inner_exclusive_access().thread_count(), 1);
        // memory_set with elf program headers/trampoline/trap context/user stack
        trace!("kernel: exec .. MemorySet::from_elf");
        let (memory_set, ustack_base, entry_point) = MemorySet::from_elf(elf_data);
        let new_token = memory_set.token();
        // substitute memory_set
        trace!("kernel: exec .. substitute memory_set");
        self.inner_exclusive_access().memory_set = memory_set;
        // then we alloc user resource for main thread again
        // since memory_set has been changed
        trace!("kernel: exec .. alloc user resource for main thread again");
        let task = self.inner_exclusive_access().get_task(0);
        let mut task_inner = task.inner_exclusive_access();
        task_inner.res.as_mut().unwrap().ustack_base = ustack_base;
        task_inner.res.as_mut().unwrap().alloc_user_res();
        task_inner.trap_cx_ppn = task_inner.res.as_mut().unwrap().trap_cx_ppn();
        // push arguments on user stack
        trace!("kernel: exec .. push arguments on user stack");
        let mut user_sp = task_inner.res.as_mut().unwrap().ustack_top();
        user_sp -= (args.len() + 1) * core::mem::size_of::<usize>();
        let argv_base = user_sp;
        let mut argv: Vec<_> = (0..=args.len())
            .map(|arg| {
                translated_refmut(
                    new_token,
                    (argv_base + arg * core::mem::size_of::<usize>()) as *mut usize,
                )
            })
            .collect();
        *argv[args.len()] = 0;
        for i in 0..args.len() {
            user_sp -= args[i].len() + 1;
            *argv[i] = user_sp;
            let mut p = user_sp;
            for c in args[i].as_bytes() {
                *translated_refmut(new_token, p as *mut u8) = *c;
                p += 1;
            }
            *translated_refmut(new_token, p as *mut u8) = 0;
        }
        // make the user_sp aligned to 8B for k210 platform
        user_sp -= user_sp % core::mem::size_of::<usize>();
        // initialize trap_cx
        trace!("kernel: exec .. initialize trap_cx");
        let mut trap_cx = TrapContext::app_init_context(
            entry_point,
            user_sp,
            KERNEL_SPACE.exclusive_access().token(),
            task.kstack.get_top(),
            trap_handler as usize,
        );
        trap_cx.x[10] = args.len();
        trap_cx.x[11] = argv_base;
        *task_inner.get_trap_cx() = trap_cx;
    }

    /// Only support processes with a single thread.
    pub fn fork(self: &Arc<Self>) -> Arc<Self> {
        trace!("kernel: fork");
        let mut parent = self.inner_exclusive_access();
        assert_eq!(parent.thread_count(), 1);
        // clone parent's memory_set completely including trampoline/ustacks/trap_cxs
        let memory_set = MemorySet::from_existed_user(&parent.memory_set);
        // alloc a pid
        let pid = pid_alloc();
        // copy fd table
        let mut new_fd_table: Vec<Option<Arc<dyn File + Send + Sync>>> = Vec::new();
        for fd in parent.fd_table.iter() {
            if let Some(file) = fd {
                new_fd_table.push(Some(file.clone()));
            } else {
                new_fd_table.push(None);
            }
        }
        // create child process pcb
        let child = Arc::new(Self {
            pid,
            inner: unsafe {
                UPSafeCell::new(ProcessControlBlockInner {
                    is_zombie: false,
                    memory_set,
                    parent: Some(Arc::downgrade(self)),
                    children: Vec::new(),
                    exit_code: 0,
                    fd_table: new_fd_table,
                    signals: SignalFlags::empty(),
                    tasks: Vec::new(),
                    task_res_allocator: RecycleAllocator::new(),
                    mutex_list: Vec::new(),
                    semaphore_list: Vec::new(),
                    condvar_list: Vec::new(),
                    deadlock_detect: false,
                    semaphore_allocation: Vec::new(),
                    mutex_allocation: Vec::new(),
                })
            },
        });
        // add child
        parent.children.push(Arc::clone(&child));
        // create main thread of child process
        let task = Arc::new(TaskControlBlock::new(
            Arc::clone(&child),
            parent
                .get_task(0)
                .inner_exclusive_access()
                .res
                .as_ref()
                .unwrap()
                .ustack_base(),
            // here we do not allocate trap_cx or ustack again
            // but mention that we allocate a new kstack here
            false,
        ));
        // attach task to child process
        let mut child_inner = child.inner_exclusive_access();
        child_inner.tasks.push(Some(Arc::clone(&task)));
        child_inner.ensure_deadlock_matrices();
        drop(child_inner);
        // modify kstack_top in trap_cx of this thread
        let task_inner = task.inner_exclusive_access();
        let trap_cx = task_inner.get_trap_cx();
        trap_cx.kernel_sp = task.kstack.get_top();
        drop(task_inner);
        insert_into_pid2process(child.getpid(), Arc::clone(&child));
        // add this thread to scheduler
        add_task(task);
        child
    }
    /// get pid
    pub fn getpid(&self) -> usize {
        self.pid.0
    }
}
