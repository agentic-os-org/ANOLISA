forbid (
    principal is SecCore::Subject,
    action == SecCore::Action::"file.read",
    resource is SecCore::File
)
when { principal has agentType && principal.agentType == "qoder" }
when { resource.path == "/work/data/secret.txt" };

@id("qoder-github")
@modify("inject_credential")
@real_token_file("/secure/github-real")
@fake_token_file("/secure/github-fake")
@inject_header("Authorization")
@inject_format("auto")
@response_rewrite("true")
permit (
    principal is SecCore::Subject,
    action == SecCore::Action::"network.send",
    resource is SecCore::HttpEndpoint
)
when {
    principal has agentType && principal.agentType == "qoder" &&
    principal has ownerUid && principal.ownerUid == 1000
}
when {
    resource.host == "api.github.com" &&
    context.input.scheme == "https" &&
    context.input.method == "GET"
};
