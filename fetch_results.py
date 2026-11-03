import sys
sys.path.insert(0, r"C:\Users\zs\Desktop\WarpGate2")
from ssh_lib import connect
import os

local_dir = r"C:\Users\zs\Desktop\WarpGate2\soak_results"
os.makedirs(local_dir, exist_ok=True)
cli = connect()
sftp = cli.open_sftp()
files = ["nativecheck.tsv", "monitor.tsv", "leak.tsv", "lane2.tsv",
         "hang_rate.tsv", "final_report.txt", "subject.txt", "loop.start"]
for f in files:
    try:
        sftp.get("/root/soak/" + f, os.path.join(local_dir, f))
        print("got", f, os.path.getsize(os.path.join(local_dir, f)))
    except Exception as e:
        print("MISS", f, e)
sftp.close()
cli.close()
