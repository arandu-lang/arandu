#include <assert.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>
#include <signal.h>

static uint8_t phase_keys[2];
static _Atomic int phase_order;
static int phase_arrivals[5];
typedef struct {
    uint8_t *key;
    int write, id;
    _Atomic int acquired, release;
} phase_worker;

static void *phase_run(void *raw) {
    phase_worker *worker = (phase_worker*)raw;
    if (worker->write) ar_rt_rwlock_write(worker->key);
    else ar_rt_rwlock_read(worker->key);
    int slot = atomic_fetch_add(&phase_order, 1);
    if (slot < 5) phase_arrivals[slot] = worker->id;
    atomic_store(&worker->acquired, 1);
    while (!atomic_load(&worker->release)) sched_yield();
    if (worker->write) ar_rt_rwlock_write_unlock(worker->key);
    else ar_rt_rwlock_read_unlock(worker->key);
    return NULL;
}

typedef struct {
    uint8_t *key;
    int write;
} phase_foreign_unlock;

static void *phase_foreign_unlock_run(void *raw) {
    phase_foreign_unlock *args = (phase_foreign_unlock*)raw;
    if (args->write) ar_rt_rwlock_write_unlock(args->key);
    else ar_rt_rwlock_read_unlock(args->key);
    return NULL;
}

static void phase_queued(uint8_t *key, size_t readers, size_t writers);

static void phase_foreign_unlock_aborts(uint8_t *key, int write) {
    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        struct rlimit no_core = {0,0};
        (void)setrlimit(RLIMIT_CORE, &no_core);
        if (write) ar_rt_rwlock_write(key);
        else ar_rt_rwlock_read(key);
        phase_foreign_unlock args = {key, write};
        pthread_t invalid_unlocker;
        if (pthread_create(&invalid_unlocker, NULL, phase_foreign_unlock_run, &args) != 0)
            _exit(10);
        (void)pthread_join(invalid_unlocker, NULL);
        _exit(0);
    }
    int status = 0;
    assert(waitpid(child, &status, 0) == child);
    assert(WIFSIGNALED(status) && WTERMSIG(status) == SIGABRT);
}

static void phase_self_deadlock_aborts(uint8_t *key, int held_write,
                                       int requested_write, int queued_writer) {
    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        struct rlimit no_core = {0,0};
        (void)setrlimit(RLIMIT_CORE, &no_core);
        if (held_write) ar_rt_rwlock_write(key);
        else ar_rt_rwlock_read(key);
        if (queued_writer) {
            phase_worker writer = {key,1,9,0,0};
            pthread_t thread;
            if (pthread_create(&thread, NULL, phase_run, &writer) != 0) _exit(11);
            phase_queued(key, 0, 1);
        }
        if (requested_write) ar_rt_rwlock_write(key);
        else ar_rt_rwlock_read(key);
        _exit(0);
    }
    int status = 0;
    assert(waitpid(child, &status, 0) == child);
    assert(WIFSIGNALED(status) && WTERMSIG(status) == SIGABRT);
}

static void phase_queued(uint8_t *key, size_t readers, size_t writers) {
    for (;;) {
        ar_cv_bucket *bucket = ar_get_cv_bucket(key);
        ar_cv_lock(bucket);
        ar_phase_state *state = *ar_phase_link(bucket, key);
        size_t r = 0, w = 0;
        if (state) {
            for (ar_cv_waiter *waiter = state->read_head; waiter; waiter = waiter->next) r++;
            for (ar_cv_waiter *waiter = state->write_head; waiter; waiter = waiter->next) w++;
        }
        ar_cv_unlock(bucket);
        if (r == readers && w == writers) return;
        sched_yield();
    }
}
static void phase_arrived(phase_worker *worker) {
    while (!atomic_load(&worker->acquired)) sched_yield();
}
static void phase_empty(uint8_t *key) {
    ar_cv_bucket *bucket = ar_get_cv_bucket(key);
    ar_cv_lock(bucket);
    assert(*ar_phase_link(bucket, key) == NULL);
    ar_cv_unlock(bucket);
}

int main(void) {
    alarm(20);
    uint8_t *key = &phase_keys[0];
    pthread_t r[3], w[2];
    assert(ar_rt_rwlock_try_read(key) == 1);
    assert(ar_rt_rwlock_try_read(key) == 1);
    assert(ar_rt_rwlock_try_write(key) == 0);
    assert(ar_rt_rwlock_try_write(&phase_keys[1]) == 1);
    ar_rt_rwlock_write_unlock(&phase_keys[1]);
    ar_rt_rwlock_read_unlock(key);
    ar_rt_rwlock_read_unlock(key);
    phase_empty(key);

    phase_self_deadlock_aborts(key, 0, 1, 0); /* read -> write */
    phase_self_deadlock_aborts(key, 1, 0, 0); /* write -> read */
    phase_self_deadlock_aborts(key, 1, 1, 0); /* write -> write */
    phase_self_deadlock_aborts(key, 0, 0, 1); /* read -> read behind writer */

    /* A current reader can reacquire while no writer is queued. */
    ar_rt_rwlock_read(key);
    ar_rt_rwlock_read(key);
    size_t owner_id = ar_rt_thread_current_id();
    ar_cv_bucket *owner_bucket = ar_get_cv_bucket(key);
    ar_cv_lock(owner_bucket);
    ar_phase_state *owner_state = *ar_phase_link(owner_bucket,key);
    assert(owner_state->readers == 2);
    assert(owner_state->reader_owners && owner_state->reader_owners->thread_id == owner_id);
    assert(owner_state->reader_owners->count == 2);
    ar_cv_unlock(owner_bucket);
    ar_rt_rwlock_read_unlock(key);
    ar_rt_rwlock_read_unlock(key);
    phase_empty(key);
    atomic_store(&phase_order,0);
    memset(phase_arrivals,0,sizeof(phase_arrivals));

    ar_rt_rwlock_write(key);
    phase_worker readers[3] = {{key,0,1,0,0}, {key,0,2,0,0}, {key,0,4,0,0}};
    phase_worker writers[2] = {{key,1,3,0,0}, {key,1,5,0,0}};
    assert(pthread_create(&r[0], NULL, phase_run, &readers[0]) == 0);
    phase_queued(key,1,0);
    assert(pthread_create(&w[0], NULL, phase_run, &writers[0]) == 0);
    phase_queued(key,1,1);
    assert(pthread_create(&r[1], NULL, phase_run, &readers[1]) == 0);
    phase_queued(key,2,1);
    assert(pthread_create(&w[1], NULL, phase_run, &writers[1]) == 0);
    phase_queued(key,2,2);
    ar_rt_rwlock_write_unlock(key);
    phase_arrived(&readers[0]); phase_arrived(&readers[1]);
    assert(atomic_load(&writers[0].acquired) == 0);
    assert(pthread_create(&r[2], NULL, phase_run, &readers[2]) == 0);
    phase_queued(key,1,2);
    atomic_store(&readers[0].release,1); atomic_store(&readers[1].release,1);
    phase_arrived(&writers[0]);
    assert(atomic_load(&readers[2].acquired) == 0);
    atomic_store(&writers[0].release,1);
    phase_arrived(&readers[2]);
    assert(atomic_load(&writers[1].acquired) == 0);
    atomic_store(&readers[2].release,1);
    phase_arrived(&writers[1]); atomic_store(&writers[1].release,1);
    for (int i=0; i<3; i++) assert(pthread_join(r[i],NULL) == 0);
    for (int i=0; i<2; i++) assert(pthread_join(w[i],NULL) == 0);
    assert((phase_arrivals[0] == 1 && phase_arrivals[1] == 2) ||
           (phase_arrivals[0] == 2 && phase_arrivals[1] == 1));
    assert(phase_arrivals[2] == 3 && phase_arrivals[3] == 4 && phase_arrivals[4] == 5);
    phase_empty(key);

    /* Cancel queued readers and writers without dangling stack waiters. */
    ar_rt_rwlock_write(key);
    phase_worker cancel_read = {key,0,6,0,0}, cancel_write = {key,1,7,0,0};
    assert(pthread_create(&r[0],NULL,phase_run,&cancel_read) == 0);
    phase_queued(key,1,0);
    assert(pthread_create(&w[0],NULL,phase_run,&cancel_write) == 0);
    phase_queued(key,1,1);
    assert(pthread_cancel(r[0]) == 0); assert(pthread_cancel(w[0]) == 0);
    void *status;
    assert(pthread_join(r[0],&status) == 0 && status == PTHREAD_CANCELED);
    assert(pthread_join(w[0],&status) == 0 && status == PTHREAD_CANCELED);
    phase_queued(key,0,0);
    ar_rt_rwlock_write_unlock(key); phase_empty(key);

    /* Race cancellation with a signaled admission. POSIX permits cond_wait
       to return normally if the notification wins; that path releases too. */
    ar_rt_rwlock_write(key);
    atomic_store(&cancel_read.release,1);
    assert(pthread_create(&r[0],NULL,phase_run,&cancel_read) == 0);
    phase_queued(key,1,0);
    ar_cv_bucket *bucket = ar_get_cv_bucket(key);
    ar_cv_lock(bucket);
    ar_phase_state *state = *ar_phase_link(bucket,key);
    ar_phase_release_state(state,1,ar_rt_thread_current_id()); /* reserve and signal */
    assert(pthread_cancel(r[0]) == 0);
    ar_cv_unlock(bucket);
    assert(pthread_join(r[0],&status) == 0);
    assert(status == PTHREAD_CANCELED || status == NULL);
    phase_empty(key);

    /* Foreign read and write unlocks fail closed. */
    phase_foreign_unlock_aborts(key, 0);
    phase_foreign_unlock_aborts(key, 1);
    return 0;
}
