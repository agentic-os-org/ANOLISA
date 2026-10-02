%define anolis_release 1
%global debug_package %{nil}

Name:           blaze
Version:        0.3.0
Release:        %{anolis_release}%{?dist}
Summary:        Per-host sandbox orchestrator daemon for AI Agent workloads

License:        Apache-2.0
URL:            https://github.com/alibaba/anolisa
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  rust >= 1.88
BuildRequires:  cargo

Provides:       anolisa-component(blaze)

%description
Blaze is the ANOLISA per-host sandbox orchestrator daemon. It manages sandbox
instance lifecycles via HTTP API with policy-driven backend selection, supporting
Firecracker microVM, Bubblewrap, and Mock backends. Features include
multi-backend fallback and Prometheus metrics export.

%prep
%setup -q

%build
cargo build --release --locked

%install
rm -rf $RPM_BUILD_ROOT
install -d -m 0755 %{buildroot}%{_libexecdir}/anolisa
install -d -m 0755 %{buildroot}%{_unitdir}
install -d -m 0755 %{buildroot}%{_sysconfdir}/anolisa/blaze/policies
install -d -m 0755 %{buildroot}%{_datadir}/anolisa/components/blaze
install -d -m 0755 %{buildroot}%{_docdir}/blaze
install -d -m 0755 %{buildroot}/run/blaze
install -d -m 0755 %{buildroot}/var/lib/blaze

install -Dm755 target/release/blazed %{buildroot}%{_libexecdir}/anolisa/blazed
install -p -m 0644 dist/blazed.service %{buildroot}%{_unitdir}/
install -Dm644 .anolisa/component.toml %{buildroot}%{_datadir}/anolisa/components/blaze/component.toml
install -p -m 0644 examples/config.toml %{buildroot}%{_sysconfdir}/anolisa/blaze/config.toml
install -p -m 0644 examples/policies/agent-rl.toml %{buildroot}%{_sysconfdir}/anolisa/blaze/policies/
install -p -m 0644 examples/policies/agent-tool.toml %{buildroot}%{_sysconfdir}/anolisa/blaze/policies/
install -p -m 0644 README.md %{buildroot}%{_docdir}/blaze/
install -p -m 0644 README_zh.md %{buildroot}%{_docdir}/blaze/
install -p -m 0644 LICENSE %{buildroot}%{_docdir}/blaze/

%post
%systemd_post blazed.service

%preun
%systemd_preun blazed.service

%postun
%systemd_postun blazed.service

%files
%defattr(0644,root,root,0755)
%attr(0755,root,root) %{_libexecdir}/anolisa/blazed
%config(noreplace) %{_sysconfdir}/anolisa/blaze/config.toml
%dir %{_sysconfdir}/anolisa/blaze/policies
%config(noreplace) %{_sysconfdir}/anolisa/blaze/policies/agent-rl.toml
%config(noreplace) %{_sysconfdir}/anolisa/blaze/policies/agent-tool.toml
%{_unitdir}/blazed.service
%{_datadir}/anolisa/components/blaze/component.toml
%dir /run/blaze
%dir /var/lib/blaze
%doc %{_docdir}/blaze/README.md
%doc %{_docdir}/blaze/README_zh.md
%license %{_docdir}/blaze/LICENSE

%changelog
* Mon Jul 21 2026 Caspar Zhang <caspar@linux.alibaba.com> - 0.2.1-1
- Rebrand: component renamed from Anvil to Blaze
- Firecracker vCPU upper bound validation (1-32)
- Register component in project manifests
- Document VM resource config fallback chain

* Mon Jun 30 2026 Caspar Zhang <caspar@linux.alibaba.com> - 0.2.0-1
- Add FirecrackerSpawner backend with auto-detection
- Add TCP remote API on port 14159
- Add prioritized backend selection
- Add packaging skeleton (systemd, RPM spec, tmpfiles)
