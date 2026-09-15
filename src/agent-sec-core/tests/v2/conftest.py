"""Options shared by the V2 pytest suites."""


def pytest_addoption(parser):
    parser.addoption(
        "--require-systemd",
        action="store_true",
        help="Fail instead of skipping system-manager tests when root/PID 1 systemd is absent",
    )
