---
name: mixed-params
description: A skill with a mix of valid and invalid parameter lines
version: 0.1.0
---

# Mixed Parameters

Test various parameter line formats.

## Parameters

- `query` (string, required): A valid parameter
- `count` (integer, optional): Another valid one
- `data` (object, required): An object parameter
- `flag` (boolean): Missing required/optional marker
- `x` (unknown_type, required): This type is invalid
- Some plain text that is not a parameter
- `broken line without proper format

## Returns

- `output` (string, required): The result
