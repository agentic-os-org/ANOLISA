// GitHub REST and Smart HTTP; every endpoint supports token and Basic carriers.
@id("github-rest-auto")
@modify("inject_credential")
@real_token_file("/secure/github-real")
@fake_token_file("/secure/github-fake")
@inject_header("Authorization")
@inject_format("auto")
@response_rewrite("true")
permit(principal, action == SecCore::Action::"network.send", resource is SecCore::HttpEndpoint)
when {
    resource.host == "api.github.com" && context.input.scheme == "https" &&
    ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"].contains(context.input.method) &&
    context.input.path like "/*"
};

@id("github-rest-basic")
@modify("inject_credential")
@real_token_file("/secure/github-real")
@fake_token_file("/secure/github-fake")
@inject_header("Authorization")
@inject_format("basic")
@response_rewrite("true")
permit(principal, action == SecCore::Action::"network.send", resource is SecCore::HttpEndpoint)
when {
    resource.host == "api.github.com" && context.input.scheme == "https" &&
    ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"].contains(context.input.method) &&
    context.input.path like "/*"
};

@id("github-refs-auto")
@modify("inject_credential")
@real_token_file("/secure/github-real")
@fake_token_file("/secure/github-fake")
@inject_header("Authorization")
@inject_format("auto")
@response_rewrite("true")
permit(principal, action == SecCore::Action::"network.send", resource is SecCore::HttpEndpoint)
when {
    resource.host == "github.com" && context.input.scheme == "https" &&
    context.input.method == "GET" && context.input.path like "/*/info/refs"
};

@id("github-refs-basic")
@modify("inject_credential")
@real_token_file("/secure/github-real")
@fake_token_file("/secure/github-fake")
@inject_header("Authorization")
@inject_format("basic")
@response_rewrite("true")
permit(principal, action == SecCore::Action::"network.send", resource is SecCore::HttpEndpoint)
when {
    resource.host == "github.com" && context.input.scheme == "https" &&
    context.input.method == "GET" && context.input.path like "/*/info/refs"
};

@id("github-upload-auto")
@modify("inject_credential")
@real_token_file("/secure/github-real")
@fake_token_file("/secure/github-fake")
@inject_header("Authorization")
@inject_format("auto")
@response_rewrite("true")
permit(principal, action == SecCore::Action::"network.send", resource is SecCore::HttpEndpoint)
when {
    resource.host == "github.com" && context.input.scheme == "https" &&
    context.input.method == "POST" && context.input.path like "/*/git-upload-pack"
};

@id("github-upload-basic")
@modify("inject_credential")
@real_token_file("/secure/github-real")
@fake_token_file("/secure/github-fake")
@inject_header("Authorization")
@inject_format("basic")
@response_rewrite("true")
permit(principal, action == SecCore::Action::"network.send", resource is SecCore::HttpEndpoint)
when {
    resource.host == "github.com" && context.input.scheme == "https" &&
    context.input.method == "POST" && context.input.path like "/*/git-upload-pack"
};

@id("github-receive-auto")
@modify("inject_credential")
@real_token_file("/secure/github-real")
@fake_token_file("/secure/github-fake")
@inject_header("Authorization")
@inject_format("auto")
@response_rewrite("true")
permit(principal, action == SecCore::Action::"network.send", resource is SecCore::HttpEndpoint)
when {
    resource.host == "github.com" && context.input.scheme == "https" &&
    context.input.method == "POST" && context.input.path like "/*/git-receive-pack"
};

@id("github-receive-basic")
@modify("inject_credential")
@real_token_file("/secure/github-real")
@fake_token_file("/secure/github-fake")
@inject_header("Authorization")
@inject_format("basic")
@response_rewrite("true")
permit(principal, action == SecCore::Action::"network.send", resource is SecCore::HttpEndpoint)
when {
    resource.host == "github.com" && context.input.scheme == "https" &&
    context.input.method == "POST" && context.input.path like "/*/git-receive-pack"
};
