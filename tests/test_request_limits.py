from concurrent.futures import ThreadPoolExecutor
import threading
import time
import unittest
from unittest.mock import patch
from server.app.ai import limits


class RequestLimitsTests(unittest.TestCase):
    def setUp(self):
        p=patch.object(limits,'_budgets',{});p.start();self.addCleanup(p.stop)

    def test_endpoint_concurrency_and_failure_release(self):
        current=0;peak=0;lock=threading.Lock()
        def work(number):
            nonlocal current,peak
            try:
                with limits.request_slot(('endpoint','image'),0,2):
                    with lock:current+=1;peak=max(peak,current)
                    time.sleep(.015)
                    with lock:current-=1
                    if number==2:raise ValueError('simulated failure')
            except ValueError:pass
        with ThreadPoolExecutor(max_workers=8) as pool:list(pool.map(work,range(8)))
        self.assertEqual(peak,2)
        with limits.request_slot(('endpoint','image'),0,2):pass
        self.assertEqual(limits._budgets[('endpoint','image')]['active'],0)

    def test_third_request_waits_for_rate_window_and_other_endpoint_is_independent(self):
        clock=[0.0];starts=[]
        def wait(_timeout):clock[0]+=1
        with patch.object(limits.time,'monotonic',side_effect=lambda:clock[0]),patch.object(limits._condition,'wait',side_effect=wait):
            for _ in range(3):
                with limits.request_slot(('one','image'),2,8):starts.append(clock[0])
            with limits.request_slot(('two','image'),2,8):other=clock[0]
        self.assertEqual(starts,[0,0,60]);self.assertEqual(other,60)


if __name__=='__main__':unittest.main()
