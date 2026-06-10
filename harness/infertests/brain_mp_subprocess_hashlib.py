import multiprocessing
import subprocess
import hashlib

v = multiprocessing.Value("i", 0)
e = multiprocessing.Event()
q = multiprocessing.Queue()
l = multiprocessing.Lock()
m = multiprocessing.Manager()
p = multiprocessing.Process(target=None)
pl = multiprocessing.Pool(2)

proc = subprocess.Popen(["ls"])
proc.terminate()
proc.kill()
out = proc.communicate()
rc = proc.wait()
co = subprocess.check_output(["ls"])

h = hashlib.md5()
h.update(b"x")
hd = h.hexdigest()
d = h.digest()
