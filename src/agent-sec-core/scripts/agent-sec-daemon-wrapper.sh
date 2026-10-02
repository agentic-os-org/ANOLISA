#!/bin/bash
# Wrapper script for agent-sec-daemon
# Sets PYTHONPATH to private site-packages to avoid conflicts with system RPM packages
PYTHONPATH=/opt/agent-sec/lib/python3.11/site-packages${PYTHONPATH:+:$PYTHONPATH} exec /usr/bin/python3.11 -c 'import sys; sys.argv[0] = "agent-sec-daemon"; from agent_sec_cli.daemon.server import main; sys.exit(main())' "$@"
