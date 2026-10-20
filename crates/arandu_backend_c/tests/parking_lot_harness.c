#include <assert.h>
#include <signal.h>

static _Atomic size_t words[65];
typedef struct {
    _Atomic size_t *key;
    _Atomic int result;
    int preserve_token;
    int indefinite;
} test_wait;

static void *wait_worker(void *arg) {
    test_wait *w = (test_wait*)arg;
    if (w->preserve_token) ar_rt_thread_unpark(ar_rt_thread_current_id());
    int result;
    if (w->indefinite) {
        ar_rt_condvar_wait(w->key, 0);
        result = 1;
    } else {
        result = ar_rt_condvar_wait_timeout(w->key, 0, 5000);
    }
    if (w->preserve_token) ar_rt_thread_park();
    atomic_store(&w->result, result);
    return NULL;
}

static void await_registration(test_wait *w) {
    for (;;) {
        ar_cv_bucket *b = ar_get_cv_bucket(w->key);
        ar_cv_lock(b);
        ar_cv_waiter *cur = b->head;
        while (cur && cur->key != w->key) cur = cur->next;
        int ready = cur != NULL;
        ar_cv_unlock(b);
        if (ready) return;
        sched_yield();
    }
}

static void *identity_worker(void *arg) {
    *(size_t*)arg = ar_rt_thread_current_id();
    return NULL;
}

typedef struct {
    _Atomic size_t id;
    uint64_t timeout_ms;
    _Atomic int done;
} timeout_test_arg;

static void *park_timeout_worker(void *arg) {
    timeout_test_arg *t = (timeout_test_arg*)arg;
    atomic_store(&t->id, ar_rt_thread_current_id());
    ar_rt_thread_park_timeout(t->timeout_ms);
    atomic_store(&t->done, 1);
    return NULL;
}

int main(void) {
    alarm(15); /* a lost wake is a bounded test failure */
    assert(ar_rt_condvar_wait_timeout(NULL, 0, 0) == -1);
    assert(ar_rt_condvar_wait_timeout(&words[0], 1, 5000) == 1);
    assert(ar_rt_condvar_wait_timeout(&words[0], 0, 0) == 0);
    assert(ar_rt_condvar_wait_timeout(&words[0], 0, 1) == 0);
    assert(ar_get_cv_bucket(&words[0])->head == NULL);
    int first = -1, second = -1;
    for (int i = 0; i < 65 && first < 0; i++) {
        for (int j = i + 1; j < 65; j++) {
            if (ar_get_cv_bucket(&words[i]) == ar_get_cv_bucket(&words[j])) {
                first = i; second = j; break;
            }
        }
    }
    assert(first >= 0);
    test_wait a = {&words[first], -2, 1}, b = {&words[second], -2, 0};
    pthread_t ta, tb;
    assert(pthread_create(&ta, NULL, wait_worker, &a) == 0);
    await_registration(&a);
    assert(pthread_create(&tb, NULL, wait_worker, &b) == 0);
    await_registration(&b);
    atomic_fetch_add(a.key, 1);
    ar_rt_condvar_notify_one(a.key);
    assert(pthread_join(ta, NULL) == 0);
    assert(atomic_load(&a.result) == 1);
    assert(atomic_load(&b.result) == -2);
    atomic_fetch_add(b.key, 1);
    ar_rt_condvar_notify_all(b.key);
    assert(pthread_join(tb, NULL) == 0);
    assert(atomic_load(&b.result) == 1);
    assert(ar_get_cv_bucket(a.key)->head == NULL);
    atomic_store(&words[0], 0);
    test_wait canceled = {&words[0], -2, 0, 1};
    pthread_t canceled_thread;
    assert(pthread_create(&canceled_thread, NULL, wait_worker, &canceled) == 0);
    await_registration(&canceled);
    assert(pthread_cancel(canceled_thread) == 0);
    void *canceled_result = NULL;
    assert(pthread_join(canceled_thread, &canceled_result) == 0);
    assert(canceled_result == PTHREAD_CANCELED);
    assert(ar_get_cv_bucket(canceled.key)->head == NULL);
    size_t last_id = 0;
    for (int i = 0; i < 32; i++) {
        size_t id = 0;
        pthread_t thread;
        assert(pthread_create(&thread, NULL, identity_worker, &id) == 0);
        assert(pthread_join(thread, NULL) == 0);
        assert(id > last_id);
        last_id = id;
        ar_waiter_list_lock();
        for (ar_thread_waiter *cur = ar_waiter_list; cur; cur = cur->next) assert(cur->id != id);
        ar_waiter_list_unlock();
        ar_rt_thread_unpark(id);
    }

    /* 1. Prior token wakes park_timeout immediately without waiting */
    ar_rt_thread_unpark(ar_rt_thread_current_id());
    ar_rt_thread_park_timeout(5000);

    /* 2. Unpark during wait interrupts park_timeout */
    timeout_test_arg t2 = {0, 5000, 0};
    pthread_t thread2;
    assert(pthread_create(&thread2, NULL, park_timeout_worker, &t2) == 0);
    while (atomic_load(&t2.id) == 0) sched_yield();
    usleep(20000); /* 20ms */
    ar_rt_thread_unpark(atomic_load(&t2.id));
    assert(pthread_join(thread2, NULL) == 0);
    assert(atomic_load(&t2.done) == 1);

    /* 3. Timeout without token expires after requested duration */
    timeout_test_arg t3 = {0, 20, 0};
    pthread_t thread3;
    assert(pthread_create(&thread3, NULL, park_timeout_worker, &t3) == 0);
    assert(pthread_join(thread3, NULL) == 0);
    assert(atomic_load(&t3.done) == 1);

    /* 4. Race timeout vs unpark terminates without deadlock */
    timeout_test_arg t4 = {0, 10, 0};
    pthread_t thread4;
    assert(pthread_create(&thread4, NULL, park_timeout_worker, &t4) == 0);
    while (atomic_load(&t4.id) == 0) sched_yield();
    usleep(10000); /* 10ms */
    ar_rt_thread_unpark(atomic_load(&t4.id));
    assert(pthread_join(thread4, NULL) == 0);
    assert(atomic_load(&t4.done) == 1);

    alarm(0);
    return 0;
}
