// Copyright 2026 Alibaba Cloud
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! L3 scenario-layer benchmark: tokenless vs the reference over whole conversations.
//!
//! Where L2 compares the two compressors on a single tool output, L3 compares
//! them on the message list an agent actually sends to a model, using the
//! scenario definitions the reference's own benchmarks use.

pub mod l3;
