import React from 'react';

export interface PaginationProps {
  total: number;
  limit: number;
  offset: number;
  onPageChange: (newOffset: number) => void;
}

function getPageNumbers(current: number, total: number): (number | '...')[] {
  if (total <= 7) return Array.from({ length: total }, (_, i) => i + 1);
  const pages: (number | '...')[] = [];
  if (current <= 4) {
    pages.push(1, 2, 3, 4, 5, '...', total);
  } else if (current >= total - 3) {
    pages.push(1, '...', total - 4, total - 3, total - 2, total - 1, total);
  } else {
    pages.push(1, '...', current - 1, current, current + 1, '...', total);
  }
  return pages;
}

export const Pagination: React.FC<PaginationProps> = ({ total, limit, offset, onPageChange }) => {
  if (total <= limit) return null;

  const currentPage = Math.floor(offset / limit) + 1;
  const totalPages = Math.max(1, Math.ceil(total / limit));
  const pages = getPageNumbers(currentPage, totalPages);

  return (
    <div className="flex items-center justify-center gap-1 py-3 px-4">
      <button
        type="button"
        disabled={currentPage <= 1}
        onClick={() => onPageChange(Math.max(0, offset - limit))}
        className="flex items-center justify-center min-w-[2rem] h-8 rounded text-sm text-gray-600 hover:bg-gray-100 disabled:text-gray-300 disabled:hover:bg-transparent"
      >
        &lt;
      </button>
      {pages.map((page, index) =>
        page === '...' ? (
          <span key={`ellipsis-${index}`} className="flex items-center justify-center min-w-[2rem] h-8 text-sm text-gray-400">
            …
          </span>
        ) : (
          <button
            key={page}
            type="button"
            onClick={() => onPageChange((page - 1) * limit)}
            className={`flex items-center justify-center min-w-[2rem] h-8 rounded text-sm font-medium ${
              page === currentPage
                ? 'bg-blue-600 text-white'
                : 'text-gray-700 hover:bg-gray-100'
            }`}
          >
            {page}
          </button>
        ),
      )}
      <button
        type="button"
        disabled={currentPage >= totalPages}
        onClick={() => onPageChange(offset + limit)}
        className="flex items-center justify-center min-w-[2rem] h-8 rounded text-sm text-gray-600 hover:bg-gray-100 disabled:text-gray-300 disabled:hover:bg-transparent"
      >
        &gt;
      </button>
    </div>
  );
};

export default Pagination;
