package org.bukkit.event.player;

import org.bukkit.event.HandlerList;
import org.bukkit.entity.Player;

public class PlayerJoinEvent extends PlayerEvent {
    private static final HandlerList handlers = new HandlerList();
    private String joinMessage;

    public PlayerJoinEvent(Player who, String joinMessage) {
        super(who);
        this.joinMessage = joinMessage;
    }
    public String getJoinMessage() { return joinMessage; }
    public void setJoinMessage(String m) { joinMessage = m; }


    @Override public HandlerList getHandlers() { return handlers; }
    public static HandlerList getHandlerList() { return handlers; }
}
