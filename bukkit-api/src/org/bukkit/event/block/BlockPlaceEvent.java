package org.bukkit.event.block;

import org.bukkit.Material;
import org.bukkit.block.Block;
import org.bukkit.entity.Player;
import org.bukkit.event.Cancellable;
import org.bukkit.event.HandlerList;

public class BlockPlaceEvent extends BlockEvent implements Cancellable {
    private static final HandlerList handlers = new HandlerList();
    private final Player player;
    private final Material placed;
    private boolean cancel;
    private boolean canBuild = true;
    public BlockPlaceEvent(Block placedBlock, Player thePlayer, Material type) { super(placedBlock); player = thePlayer; placed = type; }
    public Player getPlayer() { return player; }
    public Block getBlockPlaced() { return getBlock(); }
    /** RimeMC: the material being placed (the world still holds the old block during the event). */
    public Material getPlacedType() { return placed; }
    public boolean canBuild() { return canBuild; }
    public void setBuild(boolean b) { canBuild = b; if (!b) cancel = true; }
    @Override public boolean isCancelled() { return cancel; }
    @Override public void setCancelled(boolean c) { cancel = c; }
    @Override public HandlerList getHandlers() { return handlers; }
    public static HandlerList getHandlerList() { return handlers; }
}
