//! ページ独立処理の順序保存並列実行
//!
//! 機械語ビルドはスレッド分割、wasm は逐次。どちらも結果の順序と値は同一

/// 入力順を保つ並列 map
///
/// ctx はスレッド用の文脈をメインスレッド上で作る（スレッドごとに1回）
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn map_ordered<T, R, C>(
    items: Vec<T>,
    mut ctx: impl FnMut() -> C,
    f: impl Fn(&C, usize, T) -> R + Sync,
) -> Vec<R>
where
    T: Send,
    R: Send,
    C: Send,
{
    let n = items.len();
    let threads = std::thread::available_parallelism()
        .map(|t| t.get())
        .unwrap_or(1)
        .min(n);
    if threads <= 1 {
        let c = ctx();
        return items
            .into_iter()
            .enumerate()
            .map(|(i, t)| f(&c, i, t))
            .collect();
    }

    let chunk_len = n.div_ceil(threads);
    let mut chunks: Vec<(C, Vec<(usize, T)>)> = Vec::with_capacity(threads);
    let mut it = items.into_iter().enumerate();
    loop {
        let chunk: Vec<(usize, T)> = it.by_ref().take(chunk_len).collect();
        if chunk.is_empty() {
            break;
        }
        chunks.push((ctx(), chunk));
    }

    let f = &f;
    std::thread::scope(|s| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|(c, chunk)| {
                s.spawn(move || {
                    chunk
                        .into_iter()
                        .map(|(i, t)| f(&c, i, t))
                        .collect::<Vec<R>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    })
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn map_ordered<T, R, C>(
    items: Vec<T>,
    mut ctx: impl FnMut() -> C,
    f: impl Fn(&C, usize, T) -> R,
) -> Vec<R> {
    let c = ctx();
    items
        .into_iter()
        .enumerate()
        .map(|(i, t)| f(&c, i, t))
        .collect()
}
