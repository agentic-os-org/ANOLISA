import { promises as fs } from "node:fs";
import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdtemp,
  readFile,
  writeFile,
  rm,
  unlink,
  chmod,
  link,
  symlink,
  rename,
} from "node:fs/promises";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { createHash } from "node:crypto";
import { loadDescriptor } from "../src/deployment.mjs";
import { loadConfiguration } from "../src/configuration.mjs";
import { createQoderBridge } from "../src/qoder.mjs";
import { Instance } from "../src/client.mjs";

const execute = promisify(execFile);
const fixture = JSON.parse(
  await readFile(
    new URL("../../tests/fixtures/deployment.json", import.meta.url),
  ),
);
const binary = process.env.AW_TEST_BINARY;
async function setup(t) {
  const root = await mkdtemp("/tmp/aw-js-descriptor-");
  t.after(() => rm(root, { recursive: true, force: true }));
  const configFile = root + "/config.json";
  const descriptorFile = root + "/descriptor.json";
  const content = JSON.stringify(fixture.configuration);
  await writeFile(configFile, content, { mode: 0o600 });
  const descriptor = structuredClone(fixture.descriptor);
  descriptor.configuration_sha256 = createHash("sha256")
    .update(content)
    .digest("hex");
  await writeFile(descriptorFile, JSON.stringify(descriptor), { mode: 0o600 });
  const options = {
    configFile,
    descriptorFile,
    profile: structuredClone(descriptor.profile),
    target: "app",
  };
  return { root, descriptor, options, load: () => loadDescriptor(options) };
}
const code = (expected) => (error) =>
  error.code === expected && !error.message.includes("secret");

test("shared strict descriptor vectors and duplicate keys", async (t) => {
  const f = await setup(t);
  for (const item of fixture.invalid) {
    const value = structuredClone(f.descriptor);
    let parent = value;
    for (const part of item.path.slice(0, -1)) parent = parent[part];
    if (item.remove) delete parent[item.path.at(-1)];
    else parent[item.path.at(-1)] = item.value;
    await writeFile(f.options.descriptorFile, JSON.stringify(value));
    await assert.rejects(f.load, code(item.error), item.name);
  }
  const encoded = JSON.stringify(f.descriptor);
  for (const item of fixture.duplicate) {
    await writeFile(
      f.options.descriptorFile,
      encoded.replace(`"${item.key}":`, item.replacement),
    );
    await assert.rejects(f.load, code("sdk_descriptor_invalid"));
  }
  for (const text of [
    encoded + "false",
    "\ufeff" + encoded,
    "[]",
    encoded.replace('"endpoint":', '"endpoint":NaN,"wrong":'),
  ]) {
    await writeFile(f.options.descriptorFile, text);
    await assert.rejects(f.load, code("sdk_descriptor_invalid"));
  }
});

test("Unicode pairs and raw byte staleness", async (t) => {
  const f = await setup(t);
  f.descriptor.endpoint = "/tmp/猫😀.sock";
  await writeFile(f.options.descriptorFile, JSON.stringify(f.descriptor));
  assert.equal((await f.load()).endpoint, f.descriptor.endpoint);
  f.descriptor.endpoint = "/tmp/\ud800.sock";
  await writeFile(f.options.descriptorFile, JSON.stringify(f.descriptor));
  await assert.rejects(f.load, code("sdk_descriptor_invalid"));
  f.descriptor.endpoint = "/tmp/aw.sock";
  await writeFile(f.options.descriptorFile, JSON.stringify(f.descriptor));
  const original = await readFile(f.options.configFile);
  await writeFile(
    f.options.configFile,
    Buffer.concat([original, Buffer.from("\n")]),
  );
  await assert.rejects(f.load, code("sdk_descriptor_stale"));
});

test("profile strings follow the shared Unicode whitespace contract", async (t) => {
  const f = await setup(t);
  for (const item of fixture.unicode_profile) {
    for (const field of ["adapter", "version", "entrypoint"]) {
      const profile = { ...f.options.profile, [field]: item.value };
      const descriptor = { ...f.descriptor, profile };
      await writeFile(f.options.descriptorFile, JSON.stringify(descriptor));
      const load = () => loadDescriptor({ ...f.options, profile });
      if (item.valid) assert.equal((await load()).target, "app", item.name);
      else
        await assert.rejects(load, code("sdk_descriptor_invalid"), item.name);
    }
  }
});

test("a third profile supplies identity and service constraints without a parser", async (t) => {
  const f = await setup(t);
  const profile = {
    adapter: "fixture-agent",
    version: "fixture-hooks/v7",
    entrypoint: "embedded",
  };
  const document = structuredClone(fixture.configuration);
  document.spec.agents.app.adapter = profile.adapter;
  const content = JSON.stringify(document);
  await writeFile(f.options.configFile, content);
  f.descriptor.configuration_sha256 = createHash("sha256")
    .update(content)
    .digest("hex");
  f.descriptor.profile = profile;
  const options = { ...f.options, profile, executable: "/missing/aw" };
  for (const availability of ["native_fallback", "required"]) {
    f.descriptor.availability = availability;
    await writeFile(f.options.descriptorFile, JSON.stringify(f.descriptor));
    for (const requiresService of [false, true]) {
      const load = () => loadConfiguration({ ...options, requiresService });
      if (requiresService && availability === "native_fallback")
        await assert.rejects(load, code("sdk_descriptor_profile_mismatch"));
      else assert.equal((await load()).availability, availability);
    }
  }
  for (const key of Object.keys(profile)) {
    await assert.rejects(
      () =>
        loadConfiguration({
          ...options,
          profile: { ...profile, [key]: "different" },
        }),
      code("sdk_descriptor_profile_mismatch"),
    );
  }
  for (const invalidProfile of [
    { adapter: profile.adapter },
    { ...profile, extra: true },
  ]) {
    await assert.rejects(
      () => loadConfiguration({ ...options, profile: invalidProfile }),
      code("sdk_descriptor_profile_mismatch"),
    );
  }
});

test("parser availability combines policy and the supplied service constraint", async (t) => {
  const f = await setup(t);
  const document = structuredClone(fixture.configuration);
  for (const [enabled, required] of [
    [false, false],
    [true, false],
    [true, true],
  ]) {
    document.spec.events["input.submit"].enabled = enabled;
    document.spec.events["input.submit"].required = required;
    await writeFile(f.options.configFile, JSON.stringify(document));
    for (const requiresService of [false, true]) {
      const wiring = await loadConfiguration({
        configFile: f.options.configFile,
        profile: {
          ...f.options.profile,
          version: "fixture/v7",
          entrypoint: "embedded",
        },
        requiresService,
        executable: binary,
      });
      assert.equal(
        wiring.availability,
        requiresService || (enabled && required)
          ? "required"
          : "native_fallback",
      );
    }
  }
});

test(
  "input files reject missing, oversized and FIFO without waiting",
  { timeout: 5000 },
  async (t) => {
    const f = await setup(t);
    for (const path of [f.options.descriptorFile, f.options.configFile]) {
      const original = await readFile(path);
      await unlink(path);
      await execute("mkfifo", [path]);
      await assert.rejects(f.load, code("sdk_descriptor_file_invalid"));
      await unlink(path);
      await writeFile(path, Buffer.alloc(4 * 1024 * 1024 + 1));
      await assert.rejects(f.load, code("sdk_descriptor_file_invalid"));
      await writeFile(path, original);
      await chmod(path, 0o600);
    }
    await unlink(f.options.descriptorFile);
    await assert.rejects(f.load, code("sdk_descriptor_file_invalid"));
  },
);

test("file and parent trust retain the legacy reader boundary", async (t) => {
  const f = await setup(t);
  for (const path of [f.options.configFile, f.options.descriptorFile]) {
    for (const mode of [0o620, 0o602, 0o4644]) {
      await chmod(path, mode);
      await assert.rejects(f.load, code("sdk_descriptor_file_invalid"));
      await chmod(path, 0o600);
    }
    const alias = f.root + "/alias";
    await link(path, alias);
    await assert.rejects(f.load, code("sdk_descriptor_file_invalid"));
    await unlink(alias);
    await rename(path, alias);
    await symlink(alias, path);
    await assert.rejects(f.load, code("sdk_descriptor_file_invalid"));
    await unlink(path);
    await rename(alias, path);
  }
  for (const mode of [0o720, 0o702]) {
    await chmod(f.root, mode);
    await assert.rejects(f.load, code("sdk_descriptor_file_invalid"));
    await chmod(f.root, 0o700);
  }
  const alias = f.root + "/directory-alias";
  await symlink(f.root, alias);
  for (const descriptorFile of [
    alias + "/descriptor.json",
    f.root + "/../" + f.root.split("/").at(-1) + "/descriptor.json",
  ]) {
    await assert.rejects(
      () => loadDescriptor({ ...f.options, descriptorFile }),
      code("sdk_descriptor_file_invalid"),
    );
  }
  assert.equal((await f.load()).target, "app");
});

test("pinned parents must match the path identity captured before traversal", async (t) => {
  const f = await setup(t);
  const original = fs.stat;
  for (const changedRead of [1, 2]) {
    for (const field of ["dev", "ino"]) {
      let calls = 0;
      const mock = t.mock.method(fs, "stat", async (...args) => {
        const info = await original(...args);
        calls++;
        return calls === changedRead
          ? { ...info, [field]: info[field] + 1n }
          : info;
      });
      await assert.rejects(f.load, code("sdk_descriptor_file_invalid"));
      assert.equal(calls, changedRead);
      mock.mock.restore();
    }
  }
  assert.equal((await f.load()).target, "app");
});

test("unknown or native-ACL filesystems fail for file and pinned parent", async (t) => {
  const f = await setup(t);
  const original = fs.statfs;
  for (const rejectCall of [1, 2]) {
    for (const type of [0x6969n, 0xff534d42n, 0x65735546n, 0x12345678n]) {
      let calls = 0;
      const mock = t.mock.method(fs, "statfs", async (...args) => {
        calls++;
        return calls === rejectCall ? { type } : original(...args);
      });
      await assert.rejects(f.load, code("sdk_descriptor_file_invalid"));
      assert.equal(calls, rejectCall);
      mock.mock.restore();
    }
  }
});

test("POSIX ACL write masks are rejected while read-only access remains valid", async (t) => {
  const f = await setup(t);
  for (const path of [f.options.configFile, f.options.descriptorFile]) {
    for (const mask of [4, 6]) {
      await execute("python3", [
        "-c",
        `
import os, struct, sys
mask = int(sys.argv[2])
acl = struct.pack("<I", 2) + b"".join(struct.pack("<HHI", *entry) for entry in (
    (1, 6, 0xffffffff), (2, mask, os.geteuid() + 1), (4, 0, 0xffffffff),
    (16, mask, 0xffffffff), (32, 0, 0xffffffff)))
os.setxattr(sys.argv[1], "system.posix_acl_access", acl)
`,
        path,
        String(mask),
      ]);
      if (mask === 6)
        await assert.rejects(f.load, code("sdk_descriptor_file_invalid"));
      else assert.equal((await f.load()).target, "app");
      await execute("python3", [
        "-c",
        'import os,sys; os.removexattr(sys.argv[1], "system.posix_acl_access")',
        path,
      ]);
      await chmod(path, 0o600);
    }
  }
});

test("CLI descriptors initialize the bridge with no parser executable or native side effects", async (t) => {
  const f = await setup(t);
  let nativeCalls = 0;
  for (const enabled of [false, true]) {
    const document = structuredClone(fixture.configuration);
    document.spec.events["input.submit"].enabled = enabled;
    document.spec.events["input.submit"].required = enabled;
    document.spec.daemon.state_dir = f.root + "/state";
    await writeFile(f.options.configFile, JSON.stringify(document));
    const { stdout } = await execute(
      binary,
      [
        "config",
        "descriptor",
        "--config",
        f.options.configFile,
        "--agent",
        "app",
        "--profile",
        "qoder-public-hooks",
      ],
      { timeout: 10000 },
    );
    await writeFile(f.options.descriptorFile, stdout);
    const wiring = await loadConfiguration({
      ...f.options,
      executable: "/missing/aw",
    });
    assert.equal(wiring.inputEnabled, enabled);
    assert.equal(wiring.availability, enabled ? "required" : "native_fallback");
    const bridge = await createQoderBridge({
      ...f.options,
      availability: enabled ? "required" : "native_fallback",
      executable: "/missing/aw",
      query() {
        nativeCalls++;
        throw new Error("unexpected native Query");
      },
    });
    await bridge.close();
    await writeFile(f.options.configFile, JSON.stringify(document) + "\n");
    await assert.rejects(
      () =>
        createQoderBridge({
          ...f.options,
          executable: "/missing/aw",
          query() {
            nativeCalls++;
          },
        }),
      code("sdk_descriptor_stale"),
    );
    assert.equal(nativeCalls, 0);
  }
});

test("descriptor and parser retain explicit runtime availability", async (t) => {
  const f = await setup(t);
  const installed = [];
  t.mock.method(Instance.prototype, "listen", function () {
    installed.push(this.options.availability);
  });
  for (const [enabled, required] of [
    [false, false],
    [true, false],
    [true, true],
  ]) {
    const document = structuredClone(fixture.configuration);
    document.spec.events["input.submit"].enabled = enabled;
    document.spec.events["input.submit"].required = required;
    await writeFile(f.options.configFile, JSON.stringify(document));
    const { stdout } = await execute(
      binary,
      [
        "config",
        "descriptor",
        "--config",
        f.options.configFile,
        "--agent",
        "app",
        "--profile",
        "qoder-public-hooks",
      ],
      { timeout: 10000 },
    );
    await writeFile(f.options.descriptorFile, stdout);
    for (const descriptorFile of [undefined, f.options.descriptorFile]) {
      for (const availability of [undefined, "native_fallback", "required"]) {
        const options = {
          configFile: f.options.configFile,
          descriptorFile,
          availability,
          executable: descriptorFile ? "/missing/aw" : binary,
          query() {
            throw new Error("unexpected native Query");
          },
        };
        const count = installed.length;
        if (enabled && required && availability !== "required") {
          await assert.rejects(
            () => createQoderBridge(options),
            code("sdk_required_policy_conflict"),
          );
          assert.equal(installed.length, count);
        } else {
          const bridge = await createQoderBridge(options);
          assert.equal(installed.length, count + 1);
          assert.equal(installed.at(-1), availability ?? "native_fallback");
          await bridge.close();
        }
      }
    }
  }
});

test("omitting the descriptor retains canonical parser target selection", async (t) => {
  const f = await setup(t);
  const wiring = await loadConfiguration({
    configFile: f.options.configFile,
    profile: f.options.profile,
    executable: binary,
  });
  assert.equal(wiring.target, "app");
  assert.equal(wiring.inputEnabled, true);
  assert.equal(wiring.availability, "required");
});
