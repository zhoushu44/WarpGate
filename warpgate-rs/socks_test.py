import socket, struct, sys

def test(port, auth=None, host="httpbin.org", path="/ip"):
    try:
        s = socket.create_connection(("127.0.0.1", port), 10)
        s.settimeout(30)

        # greeting
        methods = b"\x02" if auth else b"\x00"
        s.sendall(b"\x05\x01" + methods)
        r = s.recv(2)
        if len(r) < 2:
            return "greeting: no reply"

        if auth:
            if r[1] != 2:
                return f"greeting: server wants method {r[1]}, not user/pass"
            u, p = auth
            s.sendall(bytes([1, len(u)]) + u.encode() + bytes([len(p)]) + p.encode())
            a = s.recv(2)
            if len(a) < 2 or a[1] != 0:
                return f"auth rejected: {a.hex() if a else 'none'}"

        hb = host.encode()
        s.sendall(bytes([5, 1, 0, 3, len(hb)]) + hb + struct.pack("!H", 80))
        rep = s.recv(10)
        if len(rep) < 2:
            return "connect: no reply"
        if rep[1] != 0:
            return f"connect rejected REP={rep[1]}"

        s.sendall(f"GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n".encode())
        data = b""
        while True:
            try:
                c = s.recv(4096)
            except socket.timeout:
                break
            if not c:
                break
            data += c
        s.close()
        if not data:
            return "connected but got 0 bytes"
        first = data.split(b"\r\n")[0].decode(errors="replace")
        body = data.split(b"\r\n\r\n", 1)[-1][:100]
        return f"OK {len(data)}B status=[{first}] body={body!r}"
    except Exception as e:
        return f"EXC {type(e).__name__}: {e}"

if __name__ == "__main__":
    print("auth 10011      :", test(10011, ("warpuser", "test-pass-123")))
    print("auth 10012      :", test(10012, ("warpuser", "test-pass-123")))
    print("whitelist 11011 :", test(11011))
    print("https 10011     :", test(10011, ("warpuser", "test-pass-123"), "api.ipify.org", "/"))
