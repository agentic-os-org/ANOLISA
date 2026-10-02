/* SPDX-License-Identifier: GPL-2.0 OR BSD-3-Clause */
/* Copyright (c) 2026 eunomia-bpf org. */
#ifndef __CHANNEL_BPF_H
#define __CHANNEL_BPF_H

/*
 * Process-local channels exposed as pseudo file paths in the normal IFC engine.
 * This is the useful part of the old stdio capture path, without a separate
 * demo app or output format.
 */

static __always_inline int chan_set(char *dst, int dst_sz, const char *src, int src_sz)
{
	if (dst_sz < src_sz)
		return 0;
	__builtin_memcpy(dst, src, src_sz);
	return 1;
}

static __always_inline int chan_fd_target(int fd, __u32 access, char *target, int target_sz)
{
	if ((access & TE_ACCESS_READ) && fd == 0)
		return chan_set(target, target_sz, "stdio:stdin", sizeof("stdio:stdin"));
	if ((access & TE_ACCESS_WRITE) && fd == 1)
		return chan_set(target, target_sz, "stdio:stdout", sizeof("stdio:stdout"));
	if ((access & TE_ACCESS_WRITE) && fd == 2)
		return chan_set(target, target_sz, "stdio:stderr", sizeof("stdio:stderr"));
	return 0;
}

#endif /* __CHANNEL_BPF_H */
