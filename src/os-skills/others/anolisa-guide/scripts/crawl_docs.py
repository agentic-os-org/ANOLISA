#!/usr/bin/env python3
"""
ANOLISA 文档爬取脚本
使用 markdownify 将 HTML 转换为标准 Markdown 格式
"""
import os
import sys
import requests
from bs4 import BeautifulSoup
import markdownify
import json
import time
from datetime import datetime
from pathlib import Path

BASE_URL = "https://help.aliyun.com"

# ANOLISA 文档 URL 列表
DOC_URLS = [
    ("releasenotes", "/zh/alinux/releasenotes"),
    ("agentic-os", "/zh/alinux/agentic-os"),
    ("getting-started", "/zh/alinux/agentic-os-getting-started"),
    ("cosh-usage", "/zh/alinux/how-to-use-alibaba-cloud-linux-4-agentic-edition"),
    ("configuration", "/zh/alinux/manage-configurations"),
    ("extensibility", "/zh/alinux/extensibility-for-skill-and-mcp"),
    ("agentsight", "/zh/alinux/how-to-use-agentsight"),
    ("agentseccore", "/zh/alinux/how-to-use-agentseccore"),
    ("tokenless", "/zh/alinux/how-to-use-tokenless"),
    ("ws-ckpt", "/zh/alinux/how-to-use-ws-ckpt"),
    ("deploy-openclaw", "/zh/alinux/deploy-openclaw-claude-code-in-one-step"),
    ("resize-ecs", "/zh/alinux/resize-ecs-online-in-one-sentence"),
    ("faq", "/zh/alinux/faq"),
]

def get_page_content(url):
    """获取页面 HTML 内容"""
    try:
        headers = {
            'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36',
            'Accept': 'text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8',
            'Accept-Language': 'zh-CN,zh;q=0.9,en;q=0.8',
        }
        response = requests.get(BASE_URL + url, headers=headers, timeout=30)
        response.encoding = 'utf-8'
        return response.text
    except Exception as e:
        print(f"  ❌ 获取失败: {e}")
        return None

def extract_markdown(html, url):
    """使用 markdownify 提取并转换为 Markdown 格式"""
    soup = BeautifulSoup(html, 'html.parser')
    
    # 提取标题
    title_tag = soup.find('h1')
    if not title_tag:
        title_tag = soup.find('title')
    title = title_tag.get_text(strip=True) if title_tag else "未知标题"
    # 清理标题中的网页后缀
    if ' - ' in title:
        title = title.split(' - ')[0]
    if 'Alibaba Cloud Linux' in title:
        title = title.replace(' - Alibaba Cloud Linux(Alinux)-阿里云帮助中心', '').strip()
    
    # 提取 meta 信息
    meta_modified = soup.find('meta', {'name': 'last-modified'})
    last_mod = meta_modified.get('content', '') if meta_modified else ''
    
    # 找到 markdown-body 容器
    markdown_body = soup.find('div', class_='markdown-body')
    
    if not markdown_body:
        print(f"  ⚠️ 未找到 markdown-body 容器")
        # 尝试其他容器
        for cls in ['doc-content', 'article-content', 'content']:
            markdown_body = soup.find('div', class_=cls)
            if markdown_body:
                break
    
    content = ""
    if markdown_body:
        # 清理不需要的元素
        for tag in markdown_body.find_all(['script', 'style', 'nav', 'footer', 'header']):
            tag.decompose()
        
        # 使用 markdownify 转换
        # heading_style='ATX' 使用 # 标题格式
        # bullets='- 使用 - 作为无序列表
        content = markdownify.markdownify(
            str(markdown_body),
            heading_style='ATX',
            bullets='-',
            strip=['script', 'style']
        )
        
        # 后处理：优化格式
        content = post_process_markdown(content)
    
    return {
        'url': url,
        'full_url': BASE_URL + url,
        'title': title,
        'last_modified': last_mod,
        'content': content
    }

def post_process_markdown(content):
    """Markdown 后处理：优化格式"""
    import re
    
    # 1. 清理标题中的多余加粗标记：#### **标题** → #### 标题
    content = re.sub(r'^(#{1,6}) \*{2}(.+?)\*{2}$', r'\1 \2', content, flags=re.MULTILINE)
    
    # 2. 清理标题中分散的加粗：#### **什么** **是** → #### 什么是
    content = re.sub(r'^(#{1,6}) (\*{2}[^\*]+\*{2}\s*)+', r'\1 ', content, flags=re.MULTILINE)
    # 更精确的处理
    lines = content.split('\n')
    processed_lines = []
    for line in lines:
        if re.match(r'^#{1,6} ', line):
            # 提取标题级别
            level = re.match(r'^#{1,6}', line).group()
            # 提取标题内容，去掉所有 ** 标记
            title_content = line[len(level):].strip()
            title_content = re.sub(r'\*{2}', '', title_content)
            title_content = re.sub(r'\s+', ' ', title_content).strip()
            processed_lines.append(f"{level} {title_content}")
        else:
            processed_lines.append(line)
    content = '\n'.join(processed_lines)
    
    # 3. 清理多余的空行（连续超过3个）
    while '\n\n\n\n' in content:
        content = content.replace('\n\n\n\n', '\n\n\n')
    
    # 4. 确保列表项之间没有多余空行
    content = re.sub(r'\n\n(- |\* |\d+\. )', r'\n\1', content)
    
    # 5. 修复链接中的空格
    content = re.sub(r'\[([^\]]+)\s+\]', r'[\1]', content)
    
    return content

def save_markdown_file(data, output_path):
    """保存为 Markdown 文件"""
    crawl_time = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    
    # 构建文件内容
    file_content = f"""# {data['title']}

> **爬取时间**: {crawl_time}
> **原文链接**: {data['full_url']}
> **文档更新**: {data['last_modified']}

---

{data['content']}
"""
    
    with open(output_path, 'w', encoding='utf-8') as f:
        f.write(file_content)
    
    return file_content

def crawl_all_docs(reference_dir):
    """爬取所有文档"""
    print("=" * 60)
    print("ANOLISA 文档爬取 (Markdownify v2)")
    print(f"时间: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
    print("=" * 60)
    
    results = []
    success_count = 0
    
    for filename, url in DOC_URLS:
        print(f"\n[{filename}] {url}")
        
        # 获取页面
        html = get_page_content(url)
        if not html:
            results.append({
                'filename': filename,
                'url': url,
                'status': 'failed',
                'error': '获取页面失败'
            })
            continue
        
        # 提取 Markdown
        data = extract_markdown(html, url)
        
        # 保存文件
        output_path = reference_dir / f"{filename}.md"
        save_markdown_file(data, output_path)
        
        print(f"  ✓ 已保存: {output_path}")
        print(f"  ✓ 内容长度: {len(data['content'])} 字符")
        
        results.append({
            'filename': filename,
            'url': url,
            'status': 'success',
            'title': data['title'],
            'content_length': len(data['content']),
            'last_modified': data['last_modified'],
            'crawl_time': datetime.now().isoformat()
        })
        
        success_count += 1
        time.sleep(0.5)  # 避免请求过快
    
    # 生成摘要
    summary = {
        'crawl_time': datetime.now().isoformat(),
        'total_docs': len(DOC_URLS),
        'success_count': success_count,
        'failed_count': len(DOC_URLS) - success_count,
        'results': results
    }
    
    summary_path = reference_dir / 'crawl_summary.json'
    with open(summary_path, 'w', encoding='utf-8') as f:
        json.dump(summary, f, ensure_ascii=False, indent=2)
    
    print("\n" + "=" * 60)
    print(f"爬取完成！成功: {success_count}/{len(DOC_URLS)}")
    print(f"摘要已保存: {summary_path}")
    print("=" * 60)
    
    return summary

def main():
    """主函数"""
    import argparse
    
    parser = argparse.ArgumentParser(description='ANOLISA 文档爬取脚本')
    parser.add_argument('--output-dir', type=str, help='指定输出目录（默认使用用户缓存目录）')
    args = parser.parse_args()
    
    # 确定输出目录
    if args.output_dir:
        # 用户指定输出目录
        reference_dir = Path(args.output_dir)
    else:
        # 默认使用用户缓存目录
        cache_dir = Path.home() / ".cache" / "anolisa" / "skills" / "anolisa-guide" / "reference"
        static_dir = Path("/usr/share/anolisa/skills/anolisa-guide/reference")
        
        # 优先使用用户缓存目录（用户权限可写）
        if cache_dir.exists() or os.access(cache_dir.parent, os.W_OK):
            reference_dir = cache_dir
        elif static_dir.exists() and os.access(static_dir, os.W_OK):
            # 缓存目录不可写，尝试静态目录（开发环境）
            reference_dir = static_dir
        else:
            # 使用相对路径（开发环境）
            script_dir = Path(__file__).parent
            reference_dir = script_dir.parent / 'reference'
    
    # 确保 reference 目录存在
    reference_dir.mkdir(parents=True, exist_ok=True)
    
    print(f"输出目录: {reference_dir}")
    
    # 爬取文档
    crawl_all_docs(reference_dir)

if __name__ == '__main__':
    main()
