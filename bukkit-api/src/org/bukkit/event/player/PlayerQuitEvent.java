package org.bukkit.event.player;

import org.bukkit.event.HandlerList;
import org.bukkit.entity.Player;

public class PlayerQuitEvent extends PlayerEvent {
    private static final HandlerList handlers = new HandlerList();
    private String quitMessage;

    public PlayerQuitEvent(Player who, String quitMessage) {
        super(who);
        this.quitMessage = quitMessage;
    }
    public String getQuitMessage() { return quitMessage; }
    public void setQuitMessage(String m) { quitMessage = m; }


    @Override public HandlerList getHandlers() { return handlers; }
    public static HandlerList getHandlerList() { return handlers; }
}
