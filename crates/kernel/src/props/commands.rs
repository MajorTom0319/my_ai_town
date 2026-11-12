//! 道具放置与移除的入口：校验交给 `Props`，这里串起拓扑、空间覆盖、作品与可供性。
use super::Placement;
use crate::error::{KernelError, Result};
use crate::{navigation, World};
use protocol::PlaceNear;

impl World {
    pub fn place_prop(&mut self, placement: Placement) -> Result<()> {
        self.place_prop_near(placement, None)
    }

    /// 这个编号已经有道具用着，用掉、移除了的墓碑也算：新生成的编号要避开（`short_hash`）。
    pub(crate) fn prop_id_taken(&self, id: &str) -> bool {
        self.topo.prop(id).is_some() || self.props.is_removed(id)
    }

    /// 玩家在地图上点了位置再放：near 只决定在哪片空位里挑，校验和留白与自动选点相同。
    pub fn place_prop_near(
        &mut self,
        placement: Placement,
        near: Option<&PlaceNear>,
    ) -> Result<()> {
        if let Some(old) = self.props.placed.get(&placement.instance.id) {
            ensure!(
                old == &placement,
                KernelError::props("实例编号已存在，不能覆盖")
            );
            return Ok(());
        }
        let prop = self.check_placement(&placement)?;
        if let Some(v) = navigation::plan_override(self, &prop, near)? {
            self.topo
                .install_override(v, navigation::walk_speed(self.walk_map.as_deref())?)?;
        }
        let walk_position = if self.topo.navigation.is_none() && self.walk_map.is_some() {
            Some(
                self.plan_walk_prop(&prop.locale, &self.props.walk_positions, near)
                    .ok_or_else(|| KernelError::navigation("没有可用的二维道具位置"))?,
            )
        } else {
            None
        };
        if let Some(spot) = walk_position {
            self.props.walk_positions.insert(prop.id.clone(), spot);
        }
        self.contents.capture(&prop);
        let (id, hidden) = (prop.id.clone(), placement.definition.hidden);
        self.props.insert(&mut self.topo, placement, prop);
        self.conceal_if_hidden(&id, hidden);
        self.affordances.rebuild(&self.topo);
        Ok(())
    }

    /// 试放：和 `place_prop_near` 同样的校验和选点，只算出会落在哪，不改世界。
    /// 没有空间地图的世界没有落点可给，返回 None。
    pub fn preview_placement(
        &self,
        placement: &Placement,
        near: Option<&PlaceNear>,
    ) -> Result<Option<protocol::PropPosition>> {
        let prop = self.check_placement(placement)?;
        Ok(navigation::plan_override(self, &prop, near)?.map(|v| v.prop))
    }

    /// 各地点能放东西的格子，给摆道具界面画「哪里能放」；见 `navigation::placement_grids`。
    pub fn placement_grids(&self) -> Vec<protocol::PlaceGrid> {
        navigation::placement_grids(self)
    }

    /// 放置前的校验：道具总数、地点与定义、公告栏。
    fn check_placement(&self, placement: &Placement) -> Result<crate::topology::Prop> {
        crate::props::check_room(self.topo.props().len(), 1)?;
        let prop = self.props.validate_placement(&self.topo, placement)?;
        self.topo
            .check_board(prop.board.as_deref(), &prop.locale, prop.readable.as_ref())?;
        Ok(prop)
    }

    /// 移除道具只留下墓碑：正在用它的人被中断，历史位置和路线仍然有效。
    /// 要中断的人比道具占用更广：当前在用（含招待用餐）、当前动作或队列里的动作点名了它。
    pub fn remove_prop(&mut self, id: &str) -> Result<()> {
        if self.props.is_removed(id) {
            return Ok(());
        }
        self.props.check_removal(&self.topo, &self.inventory, id)?;
        let affected: Vec<_> = self
            .actors
            .values()
            .filter(|a| {
                crate::physiology::body_spot(self, a).is_some_and(|s| s.prop == id)
                    || a.current.action.as_ref().is_some_and(|b| b.uses_prop(id))
                    || a.plan
                        .iter()
                        .any(|q| q.binding().is_some_and(|b| b.uses_prop(id)))
            })
            .map(|a| a.id.clone())
            .collect();
        for actor in affected {
            self.interrupt_actions(&actor, "目标道具已移除");
        }
        self.inventory.forget(id);
        self.props.withdraw(&mut self.topo, id);
        self.affordances.rebuild(&self.topo);
        self.topo.bump_navigation_revision();
        Ok(())
    }

    /// 玩法把镇上一件带有限补给的道具设成指定余量（公共粮仓的米数）。
    /// 先按实例编号找，找不到再按地点里第一个带这个动作、还在地图上的道具找；
    /// 普通补给不能超过初始量；玩法管理的展示量以账本为准。返回设好之后的道具编号。
    pub fn set_map_supply(
        &mut self,
        instance: Option<&str>,
        locale: &str,
        action: &str,
        remaining: u32,
    ) -> Result<Option<String>> {
        let find = |p: &&crate::topology::Prop| {
            p.verbs
                .iter()
                .filter_map(|v| v.action.as_ref())
                .any(|a| a.id == action && a.supply.is_some())
        };
        let prop = match instance {
            Some(id) => self.topo.prop(id).filter(find).map(|p| p.id.clone()),
            None => self
                .topo
                .props()
                .iter()
                .filter(|p| p.locale == locale)
                .filter(find)
                .find(|p| !p.withdrawn && self.inventory.on_map(&self.props, &p.id))
                .map(|p| p.id.clone()),
        };
        let Some(id) = prop else {
            return Ok(None);
        };
        let Some(act) = self.topo.prop(&id).and_then(|p| {
            p.verbs
                .iter()
                .filter_map(|v| v.action.as_ref())
                .find(|a| a.id == action)
                .cloned()
        }) else {
            return Ok(None);
        };
        let value = if act.mode_managed {
            remaining
        } else {
            remaining.min(act.supply.as_ref().map_or(remaining, |s| s.initial))
        };
        if self.props.supply_remaining(&id, &act) == Some(value) {
            return Ok(Some(id));
        }
        self.props.set_supply(&id, action, value);
        self.inventory.bump();
        Ok(Some(id))
    }
}
