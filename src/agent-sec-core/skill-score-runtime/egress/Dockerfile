FROM gogost/gost:3.2.7-nightly.20260602@sha256:afc0137758ab4ce399d47a299f9abbacbf522b52a17e59cbb4b4e7a1a66e9196
RUN apk add --no-cache iptables ip6tables nftables
COPY install-rules.sh /usr/local/bin/asc-egress-install
COPY entrypoint.sh /usr/local/bin/asc-egress-entrypoint
COPY healthcheck.sh /usr/local/bin/asc-egress-healthcheck
COPY LICENSE NOTICE /usr/share/licenses/asc-egress/
RUN mkdir -p /etc/asc-egress && chmod 755 /usr/local/bin/asc-egress-*
HEALTHCHECK --interval=1s --timeout=2s --start-period=10s --retries=5 CMD ["/usr/local/bin/asc-egress-healthcheck"]
ENTRYPOINT ["/usr/local/bin/asc-egress-entrypoint"]
