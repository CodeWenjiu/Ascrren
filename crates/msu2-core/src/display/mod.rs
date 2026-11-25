//! 屏幕帧的 tile 差分与流式命令编码。
//!
//! tile 流式路径已在实机验证可用：每页先发 `02 04` 填充字，只对与填充字
//! 不同的 word 发 `04`，再 flush。不要改回纯 `04` 逐 word 发送，那会卡死固件。

use std::collections::HashMap;

use crate::error::Error;
use crate::protocol;

/// 已确认的 MSU2 面板宽度（像素）。
pub const DISPLAY_WIDTH: u16 = 160;
/// 已确认的 MSU2 面板高度（像素）。
pub const DISPLAY_HEIGHT: u16 = 80;
/// 默认 tile 尺寸。
pub const DEFAULT_TILE_SIZE: u16 = 16;

/// 屏幕上的矩形区域。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn right(self) -> u16 {
        self.x.saturating_add(self.width)
    }

    pub fn bottom(self) -> u16 {
        self.y.saturating_add(self.height)
    }
}

/// tile 网格坐标，不是像素坐标。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tile {
    pub x: u16,
    pub y: u16,
}

/// 将 160×80 framebuffer 按固定 tile 尺寸做差分。
#[derive(Debug, Clone, Copy)]
pub struct TileGrid {
    width: u16,
    height: u16,
    tile_size: u16,
    tiles_x: u16,
    tiles_y: u16,
}

impl TileGrid {
    pub fn new(width: u16, height: u16, tile_size: u16) -> Result<Self, Error> {
        if width == 0 || height == 0 || tile_size == 0 {
            return Err(Error::Protocol(
                "display dimensions and tile size must be non-zero".to_owned(),
            ));
        }

        Ok(Self {
            width,
            height,
            tile_size,
            tiles_x: width.div_ceil(tile_size),
            tiles_y: height.div_ceil(tile_size),
        })
    }

    pub fn width(self) -> u16 {
        self.width
    }

    pub fn height(self) -> u16 {
        self.height
    }

    pub fn tile_size(self) -> u16 {
        self.tile_size
    }

    pub fn tiles_x(self) -> u16 {
        self.tiles_x
    }

    pub fn tiles_y(self) -> u16 {
        self.tiles_y
    }

    pub fn tile_rect(self, tile: Tile) -> Rect {
        let x = tile.x.saturating_mul(self.tile_size);
        let y = tile.y.saturating_mul(self.tile_size);
        Rect {
            x,
            y,
            width: self.tile_size.min(self.width.saturating_sub(x)),
            height: self.tile_size.min(self.height.saturating_sub(y)),
        }
    }

    /// 把脏 tile 合并成尽量少的像素矩形。
    ///
    /// 先按行合并相邻 tile，再把行号连续且水平范围相同的段叠成矩形。
    /// 合并后的宽高仍是 tile 尺寸的整数倍，因此数据量始终按 256 字节页
    /// 对齐，不会掉进尾部不满页的低效路径。
    pub fn merge_tiles(&self, tiles: &[Tile]) -> Vec<Rect> {
        if tiles.is_empty() {
            return Vec::new();
        }

        let tiles_x = usize::from(self.tiles_x);
        let mut dirty = vec![false; tiles_x * usize::from(self.tiles_y)];
        for tile in tiles {
            let (x, y) = (usize::from(tile.x), usize::from(tile.y));
            if x < tiles_x && y < usize::from(self.tiles_y) {
                dirty[y * tiles_x + x] = true;
            }
        }

        // 每一行内把连续脏 tile 合成一个水平段。
        let mut runs: Vec<(usize, usize, usize)> = Vec::new();
        for y in 0..usize::from(self.tiles_y) {
            let row = &dirty[y * tiles_x..(y + 1) * tiles_x];
            let mut x = 0;
            while x < row.len() {
                if !row[x] {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < row.len() && row[x] {
                    x += 1;
                }
                runs.push((start, x, y));
            }
        }

        // 相同水平范围、连续行号的段继续向下延伸。逐行推进保证空行会
        // 断开合并。
        let mut merged: Vec<(usize, usize, usize, usize)> = Vec::new();
        let mut previous_row: HashMap<(usize, usize), usize> = HashMap::new();
        let mut cursor = 0usize;
        for y in 0..usize::from(self.tiles_y) {
            let mut current_row: HashMap<(usize, usize), usize> = HashMap::new();
            while cursor < runs.len() && runs[cursor].2 == y {
                let (x0, x1, _) = runs[cursor];
                if let Some(&index) = previous_row.get(&(x0, x1)) {
                    merged[index].3 = y + 1;
                    current_row.insert((x0, x1), index);
                } else {
                    merged.push((x0, x1, y, y + 1));
                    current_row.insert((x0, x1), merged.len() - 1);
                }
                cursor += 1;
            }
            previous_row = current_row;
        }

        merged
            .into_iter()
            .map(|(x0, x1, y0, y1)| self.tile_span_rect(x0, x1, y0, y1))
            .collect()
    }

    fn tile_span_rect(&self, x0: usize, x1: usize, y0: usize, y1: usize) -> Rect {
        let x = (x0 as u16).saturating_mul(self.tile_size);
        let y = (y0 as u16).saturating_mul(self.tile_size);
        let right = ((x1 as u16).saturating_mul(self.tile_size)).min(self.width);
        let bottom = ((y1 as u16).saturating_mul(self.tile_size)).min(self.height);
        Rect {
            x,
            y,
            width: right.saturating_sub(x),
            height: bottom.saturating_sub(y),
        }
    }

    /// 返回发生变化的 tile。两个 framebuffer 都必须是 RGB565，长度严格等于
    /// `width * height`。
    pub fn diff(&self, previous: &[u16], current: &[u16]) -> Result<Vec<Tile>, Error> {
        let expected = usize::from(self.width) * usize::from(self.height);
        if previous.len() != expected || current.len() != expected {
            return Err(Error::Protocol(format!(
                "framebuffer length mismatch: expected {expected}, got {} and {}",
                previous.len(),
                current.len()
            )));
        }

        let mut dirty = vec![false; usize::from(self.tiles_x) * usize::from(self.tiles_y)];
        for y in 0..usize::from(self.height) {
            for x in 0..usize::from(self.width) {
                let index = y * usize::from(self.width) + x;
                if previous[index] != current[index] {
                    let tile_x = x / usize::from(self.tile_size);
                    let tile_y = y / usize::from(self.tile_size);
                    dirty[tile_y * usize::from(self.tiles_x) + tile_x] = true;
                }
            }
        }

        Ok(dirty
            .into_iter()
            .enumerate()
            .filter_map(|(index, dirty)| {
                dirty.then_some(Tile {
                    x: (index % usize::from(self.tiles_x)) as u16,
                    y: (index / usize::from(self.tiles_x)) as u16,
                })
            })
            .collect())
    }
}

mod encode;
pub use encode::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diffs_changed_tiles() {
        let grid = TileGrid::new(160, 80, 16).unwrap();
        let previous = vec![0u16; 160 * 80];
        let mut current = previous.clone();

        assert!(grid.diff(&previous, &current).unwrap().is_empty());

        current[3 * 160 + 17] = 0xF800;
        assert_eq!(
            grid.diff(&previous, &current).unwrap(),
            vec![Tile { x: 1, y: 0 }]
        );

        current[79 * 160 + 159] = 0x07E0;
        let mut tiles = grid.diff(&previous, &current).unwrap();
        tiles.sort_by_key(|tile| (tile.y, tile.x));
        assert_eq!(tiles, vec![Tile { x: 1, y: 0 }, Tile { x: 9, y: 4 },]);
    }

    #[test]
    fn rejects_bad_framebuffer_lengths() {
        let grid = TileGrid::new(160, 80, 16).unwrap();
        let err = grid.diff(&[], &[]).unwrap_err();
        assert!(matches!(err, Error::Protocol(_)));
    }
    #[test]
    fn merges_horizontal_and_vertical_tile_runs() {
        let grid = TileGrid::new(160, 80, 16).unwrap();
        let tiles = vec![
            Tile { x: 1, y: 2 },
            Tile { x: 2, y: 2 },
            Tile { x: 1, y: 3 },
            Tile { x: 2, y: 3 },
            Tile { x: 7, y: 0 },
        ];
        let mut rects = grid.merge_tiles(&tiles);
        rects.sort_by_key(|rect| (rect.y, rect.x));

        assert_eq!(
            rects,
            vec![
                Rect {
                    x: 112,
                    y: 0,
                    width: 16,
                    height: 16,
                },
                Rect {
                    x: 16,
                    y: 32,
                    width: 32,
                    height: 32,
                },
            ]
        );
    }

    #[test]
    fn merge_keeps_non_adjacent_rows_apart() {
        let grid = TileGrid::new(160, 80, 16).unwrap();
        let rects = grid.merge_tiles(&[
            Tile { x: 0, y: 0 },
            Tile { x: 0, y: 2 },
            Tile { x: 0, y: 3 },
        ]);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].height, 16);
        assert_eq!(rects[1].height, 32);
    }

    #[test]
    fn merged_rects_stay_page_aligned() {
        let grid = TileGrid::new(160, 80, 16).unwrap();
        let mut tiles = Vec::new();
        for y in 0..5 {
            for x in 0..10 {
                if (x * 7 + y * 3) % 4 == 0 {
                    tiles.push(Tile { x, y });
                }
            }
        }

        // 末尾半行/半列不是 tile 整数倍时可能不满页，这里只检查内部 tile。
        for rect in grid.merge_tiles(&tiles) {
            assert_eq!(rect.width % 16, 0);
            assert_eq!(rect.height % 16, 0);
            let bytes = usize::from(rect.width) * usize::from(rect.height) * 2;
            assert_eq!(bytes % 256, 0, "rect {rect:?} is not page aligned");
        }
    }
}
