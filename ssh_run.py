import sys, paramiko, io

HOST = "45.205.27.41"
PORT = 22
USER = "root"
PASS = "xxlnKUDX2993"


def main():
    cmd = sys.argv[1]
    timeout = int(sys.argv[2]) if len(sys.argv) > 2 else 3600
    cli = paramiko.SSHClient()
    cli.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    cli.connect(HOST, port=PORT, username=USER, password=PASS,
                timeout=30, banner_timeout=30, auth_timeout=30,
                look_for_keys=False, allow_agent=False)
    tr = cli.get_transport()
    tr.set_keepalive(15)
    ch = tr.open_session()
    ch.get_pty()
    ch.settimeout(timeout)
    ch.exec_command(cmd)
    out = io.BytesIO()
    while True:
        try:
            data = ch.recv(65536)
        except Exception as e:
            sys.stderr.write("\n[ssh read error: %s]\n" % e)
            break
        if not data:
            break
        out.write(data)
    try:
        code = ch.recv_exit_status()
    except Exception:
        code = -1
    sys.stdout.buffer.write(out.getvalue())
    sys.stdout.flush()
    sys.stderr.write("\n[exit code: %s]\n" % code)
    cli.close()


if __name__ == "__main__":
    main()
