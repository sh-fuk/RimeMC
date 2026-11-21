package org.bukkit.event.block;

import org.bukkit.block.Block;
import org.bukkit.entity.Player;
import org.bukkit.event.Cancellable;
import org.bukkit.event.HandlerList;

public class BlockBreakEvent extends BlockEvent implements Cancellable {
    private static final HandlerList handlers = new HandlerList();
    private final Player player;
    private boolean cancel;
    private boolean dropItems = true;
    public BlockBreakEvent(Block theBlock, Player player) { super(theBlock); this.player = player; }
    public Player getPlayer() { return player; }
    public boolean isDropItems() { return dropItems; }
    public void setDropItems(boolean d) { dropItems = d; }
    @Override public boolean isCancelled() { return cancel; }
    @Override public void setCancelled(boolean c) { cancel = c; }
    @Override public HandlerList getHandlers() { return handlers; }
    public static HandlerList getHandlerList() { return handlers; }
}
