package org.bukkit.event.player;

import org.bukkit.event.HandlerList;
import org.bukkit.event.Cancellable;
import org.bukkit.entity.Player;

public class PlayerCommandPreprocessEvent extends PlayerEvent implements Cancellable {
    private static final HandlerList handlers = new HandlerList();
    private String message;
    private boolean cancel;
    public PlayerCommandPreprocessEvent(Player who, String message) {
        super(who);
        this.message = message;
    }
    public String getMessage() { return message; }
    public void setMessage(String m) { message = m; }
    @Override public boolean isCancelled() { return cancel; }
    @Override public void setCancelled(boolean c) { cancel = c; }
    @Override public HandlerList getHandlers() { return handlers; }
    public static HandlerList getHandlerList() { return handlers; }
}
