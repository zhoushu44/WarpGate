import sys, paramiko, os

HOST, PORT, USER, PASS = "45.205.27.41", 22, "root", "xxlnKUDX2993"


def main():
    local, remote = sys.argv[1], sys.argv[2]
    cli = paramiko.SSHClient()
    cli.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    cli.connect(HOST, port=PORT, username=USER, password=PASS,
                timeout=30, banner_timeout=30, auth_timeout=30,
                look_for_keys=False, allow_agent=False)
    sftp = cli.open_sftp()
    sftp.put(local, remote)
    sftp.close()
    cli.close()
    print("uploaded", local, "->", remote, os.path.getsize(local), "bytes")


if __name__ == "__main__":
    main()
