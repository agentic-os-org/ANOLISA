---
name: web-search
description: Search the web for current information on any topic
version: 1.2.0
tags:
  - search
  - web
  - information
enabled: true
---

# Web Search

Search the web for current information using multiple search engines.

## Parameters

- `query` (string, required): The search query
- `count` (integer, optional): Maximum number of results to return
- `language` (string, optional): Language code for results filtering

## Returns

- `results` (array, required): List of search result objects
- `total` (integer, required): Total number of matches found

## Examples

```
web-search --query "Rust FUSE filesystem" --count 5
```
