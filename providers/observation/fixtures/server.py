"""Loopback TLS receiver; retains only byte counts and SHA-256 digests."""

import hashlib
import socket
import ssl
import threading


class Server:
    def __init__(self, certificate, key):
        self.context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self.context.load_cert_chain(certificate, key)
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(32)
        self.listener.settimeout(0.1)
        self.port = self.listener.getsockname()[1]
        self.stopping = threading.Event()
        self.results = []
        self.errors = []
        self.workers = []
        self.thread = threading.Thread(target=self.accept)
        self.thread.start()

    def accept(self):
        while not self.stopping.is_set():
            try:
                connection, _ = self.listener.accept()
            except socket.timeout:
                continue
            worker = threading.Thread(target=self.receive, args=(connection,))
            self.workers.append(worker)
            worker.start()

    def receive(self, connection):
        count = 0
        digest = hashlib.sha256()
        try:
            connection.settimeout(10)
            with self.context.wrap_socket(connection, server_side=True) as stream:
                while True:
                    data = stream.recv(8192)
                    if not data:
                        break
                    count += len(data)
                    digest.update(data)
                stream.unwrap().close()
            self.results.append({"length_bytes": count, "sha256": digest.hexdigest()})
        except (OSError, ssl.SSLError) as error:
            self.errors.append(type(error).__name__)
        finally:
            connection.close()

    def close(self):
        self.stopping.set()
        self.thread.join(2)
        self.listener.close()
        for worker in self.workers:
            worker.join(12)
        if self.thread.is_alive() or any(w.is_alive() for w in self.workers):
            raise RuntimeError("TLS receiver cleanup deadline")
